/*
 * zig-cc-wrapper —— 给「Windows 上用 zig 交叉编译 Linux 静态二进制」用的极简转发器。
 *
 * 为什么需要它：
 *   Rust 的 musl target 叫 `x86_64-unknown-linux-musl`，而 **zig 不认 vendor=unknown
 *   的写法**，cc-rs 又一定会往编译器命令行里追加 `--target=$TARGET`。这个 wrapper
 *   把 `--target=x86_64-unknown-linux-musl` 换成 zig 认得的 `--target=x86_64-linux-musl`，
 *   其余参数原样转发给 zig。
 *
 * 顺手干的两件事（都只在 **链接** 时生效）：
 *   1. 补 `-nostdlib`；
 *   2. 丢掉 rustc 那份 **self-contained** 启动文件目录（rcrt1.o / crti.o /
 *      crtendS.o / crtn.o），让 zig 的 musl crt 当唯一来源。
 *      否则：rustc 已传 `-nostartfiles`，zig 对 musl 还会再补一份 crt1.o/crti.o，
 *      两边各定义一遍 _start / _start_c / _init / _fini ⇒ ld.lld duplicate symbol。
 *
 * ⚠️ 四个坑，别踩：
 *   1. 不能写成 .sh / .cmd：cc-rs / cargo 在 Windows 上直接 CreateProcess CC 值，
 *      脚本和批处理都 exec 不起来（os error 193 / 不是有效的 Win32 程序）。
 *   2. 传给 _spawnv 的数组，**第 0 项是子进程的 argv[0]（程序名）**，子命令在
 *      argv[1]。把 "cc" 塞 args[0] 会让 zig 把 `-O3` 当成子命令（报
 *      `unknown command: -O3`），看着像「没加 cc」，极其误导。
 *   3. zig 全局缓存按「源内容 + 参数」复用产物：改了源码但参数一样时 zig 会把
 *      旧 exe 直接拿回来（字节数一模一样）。脚本编译前先 `rm -f` 再加随机
 *      `FORCE_REBUILD` 打 bust cache。
 *   4. rustc 命令行很长时把参数写进**响应文件**再传 `@<file>`：只扫命令行自己是
 *      永远扫不到 `.rlib` 和 `rcrt1.o` 的（第一版就只扫命令行，白忙一轮）。
 *      所以这里遇到 `@` 会把文件读进来，按空白切分后再逐条处理。
 *
 * 编译（由 tools/build_linux_cross.sh 自动完成）：
 *   zig cc -target x86_64-windows-gnu -O2 -DZIG_PATH='"…/zig.exe"' \
 *          -DDEFAULT_TARGET='"x86_64-linux-musl"' \
 *          tools/zig-cc-wrapper.c -o build/zig-cc-x86_64.exe
 *   # aarch64 那份换 -DDEFAULT_TARGET 再编一次
 */
#define _CRT_SECURE_NO_WARNINGS
#include <process.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <io.h>
#include <unistd.h>
#include <windows.h>
#include <errno.h>

#ifndef ZIG_PATH
#define ZIG_PATH "zig.exe"
#endif

/* 没有 --target= 时补上的默认目标（rustc 链接期不传 --target） */
#ifndef DEFAULT_TARGET
#define DEFAULT_TARGET ""
#endif

/* ⚠️ rustc 的链接命令行展开响应文件后轻松上百个 token（objects + 上百个 rlib +
 *    -l 系列 + -o 等），数组给小了会把后半截参数静默截掉 —— 症状就是一堆莫名其妙
 *    的 undefined symbol。 */
#define MAXTOK 2048
#define MAXOUT 2048

static const char *const RENAME[][2] = {
    {"--target=x86_64-unknown-linux-musl", "--target=x86_64-linux-musl"},
    {"--target=aarch64-unknown-linux-musl", "--target=aarch64-linux-musl"},
};

/* 链接期要从参数里剔掉的 rustc self-contained 启动文件 */
static const char *const DROP_CRT[] = {
    "rcrt1.o", "crt1.o", "crti.o", "crtbeginS.o", "crtbegin.o",
    "crtendS.o", "crtn.o", "crtend.o",
};

static int ends_with(const char *s, const char *suffix) {
    size_t n = strlen(s), m = strlen(suffix);
    return n >= m && !strcasecmp(s + n - m, suffix);
}

/* 按空白切分；返回 token 个数（overflow 时截断，末尾补 NULL） */
static int split(char *buf, char *tok[MAXTOK]) {
    int n = 0;
    char *p = buf;
    while (*p && n < MAXTOK - 1) {
        while (*p == ' ' || *p == '\t' || *p == '\n' || *p == '\r') p++;
        if (!*p) break;
        tok[n++] = p;
        while (*p && *p != ' ' && *p != '\t' && *p != '\n' && *p != '\r') p++;
        if (*p) *p++ = '\0';
    }
    tok[n] = NULL;
    return n;
}

/* 读 @响应文件；找不到就返回 0（那就只按命令行处理） */
static int read_response(char *path, char *buf, size_t cap, char *tok[MAXTOK]) {
    FILE *f = fopen(path, "rb");
    if (!f) return 0;
    size_t got = fread(buf, 1, cap - 1, f);
    buf[got] = '\0';
    fclose(f);
    return split(buf, tok);
}

int main(int argc, char **argv) {
    char *args[MAXOUT];
    char *tok[MAXTOK];
    int n = 0, ntok;
    int saw_target = 0;
    int link = 0;

    /* 先决定这是不是 rustc 的链接步骤：命令行里有 @xxx 或 .rlib */
    for (int i = 1; i < argc; i++) {
        if (!strncmp(argv[i], "--target=", 10)) saw_target = 1;
        if (argv[i][0] == '@' || ends_with(argv[i], ".rlib")) link = 1;
    }

    /*
     * 取参数源：把命令行的 @响应文件 **就地展开** 成它的内容。
     * ⚠️ 千万别用「响应文件内容整体替换命令行」那套：rustc 只是把参数的一部分
     *    （前 60 来个：objects + 前几十个 rlib）写进响应文件，剩下的一堆
     *    （-lc / -L / -o / --gc-sections / 剩余 rlib）还在命令行上。整体替换会
     *    把这些全丢掉，链接结果就是莫名其妙的 undefined symbol。
     */
    int from_file = 0;
    char orig_resp[1024] = "";   /* rustc 那个 @响应文件的路径（改写目标） */
    char respbuf[1 << 16];
    char *rtok[MAXTOK];
    ntok = 0;
    for (int i = 1; i < argc && ntok < MAXTOK - 1; i++) {
        if (argv[i][0] == '@') {
            snprintf(orig_resp, sizeof(orig_resp), "%s", argv[i] + 1);
            /* read_response 内部已经按空白切完了（会往 buffer 里写 '\0'），
               返回值就是 token 数 —— 别再切一次，第二次会把 token 全吃。 */
            int cnt = read_response(argv[i] + 1, respbuf, sizeof(respbuf), rtok);
            if (cnt > 0) {
                for (int k = 0; k < cnt && ntok < MAXTOK - 1; k++) tok[ntok++] = rtok[k];
                from_file = 1;
                if (getenv("ZIG_CC_LOG")) {   /* 调试：把响应文件原文也落盘 */
                    FILE *g = fopen(getenv("ZIG_CC_LOG"), "a");
                    if (g) { fprintf(g, "---- RESPONSE %s ----\n", argv[i]); fclose(g); }
                }
                continue;
            }
        }
        tok[ntok++] = argv[i];
    }
    tok[ntok] = NULL;

    args[n++] = ZIG_PATH;   /* argv[0] = 程序名 */
    args[n++] = "cc";       /* argv[1] = 子命令 */

    /* 两种参数源下第 0 个 token 都不是真参数：命令行时它是程序名，响应文件时它
     * 是 rustc 塞进去的链接器自身路径（当输入文件喂给 zig 会 "file not recognized"）。
     * 所以两种情况都从第 1 个开始。 */
    for (int i = 1; i < ntok && n < MAXOUT - 3; i++) {
        char *a = tok[i];
        if (!a || !*a) continue;
        if (!strncmp(a, "--target=", 10)) saw_target = 1;
        if (link) {
            int drop = 0;
            for (size_t k = 0; k < sizeof(DROP_CRT) / sizeof(DROP_CRT[0]); k++) {
                if (ends_with(a, DROP_CRT[k])) { drop = 1; break; }
            }
            if (drop) continue;
        }
        for (size_t k = 0; k < sizeof(RENAME) / sizeof(RENAME[0]); k++) {
            if (!strcmp(a, RENAME[k][0])) { args[n++] = (char *)RENAME[k][1]; goto next; }
        }
        args[n++] = a;
    next:;
    }
    if (DEFAULT_TARGET[0] && !saw_target) args[n++] = "--target=" DEFAULT_TARGET;
    if (link) args[n++] = "-nostdlib";
    args[n] = NULL;

    /* 调试用：落盘转发参数。查「改了 wrapper 为啥没生效」时先看这个 */
    if (getenv("ZIG_CC_LOG")) {
        FILE *f = fopen(getenv("ZIG_CC_LOG"), "a");
        if (f) {
            fprintf(f, "=== zig-cc-wrapper (pid=%lu) link=%d from_file=%d ===\n",
                    (unsigned long)_getpid(), link, from_file);
            for (int i = 0; i < n; i++) fprintf(f, "%s\n", args[i]);
            fclose(f);
            /* ⚠️ 把 zig 的 stderr 也接到这个文件上：rustc 把链接器 stderr 吞掉后
             *    只打印一句 `exit code: 0xffffffff`，真正的 `ld.lld: error:` 全靠它。
             *    排查「wrapper 到底干了啥 / zig 到底报了啥」必看这个日志。 */
            fflush(f);
            fflush(NULL);
            dup2(fileno(f), 2);
        }
    }

    static char *final[MAXOUT];
    int rc;

    /*
     * ⚠️ 两条硬约束决定了这里的写法：
     *   1. **CreateProcess 命令行上限 32767 字符**。rustc 靠 `@响应文件` 绕开它，
     *      如果我们把响应文件**就地展开**再传给 zig，命令行会膨胀到 6~10 万字符，
     *      `_spawnv` 直接失败返回 -1（errno=EINVAL），zig 一行错都没打，rustc 那边
     *      只剩一句 `exit code: 0xffffffff` —— 极难定位。
     *   2. **zig 会把喂给它的 `@响应文件` 原样透传给 linker**（内容一大就报
     *      `lld-link: error: <文件>: unknown file type`），也就是 *另开一个* 新
     *      响应文件这条路走不通。
     *  ⇒ 唯一稳的做法：**就地改写 rustc 那个响应文件**（删 crt、改 --target），
     *    然后命令行只留 `@<原路径>` + 几个短参数。
     */
    if (from_file && orig_resp[0]) {
        FILE *rf = fopen(orig_resp, "wb");
        if (rf) {
            /* ⚠️ args[0]=zig.exe / args[1]="cc" 只是给 zig 的，不能写进响应文件，
             *    从 args[2] 起才是真正的参数（tok[1..] + 补的 --target/-nostdlib）。 */
            for (int i = 2; i < n; i++) fprintf(rf, "%s\n", args[i]);
            fclose(rf);
        }
        /* ⚠️ `@` 后面的路径一定要用正斜杠：rustc 给的是 `E:\...` 这种 Windows
         *    路径，zig 的驱动会把反斜杠当成转义/分隔符，报
         *    `unable to read response file '': IsDir`。 */
        char atpath[1024];
        snprintf(atpath, sizeof(atpath), "@%s", orig_resp);
        for (char *p = atpath + 1; *p; p++) if (*p == '\\') *p = '/';
        final[0] = (char *)ZIG_PATH;
        final[1] = "cc";
        final[2] = atpath;
        final[3] = NULL;
        if (getenv("ZIG_CC_LOG")) {
            FILE *f = fopen(getenv("ZIG_CC_LOG"), "a");
            if (f) {
                fprintf(f, "=== rewritten %s (%d args) ===\n", orig_resp, n - 1);
                fclose(f);
            }
        }
        rc = _spawnv(_P_WAIT, ZIG_PATH, (const char *const *)final);
        if (getenv("ZIG_CC_LOG")) {
            FILE *f = fopen(getenv("ZIG_CC_LOG"), "a");
            if (f) { fprintf(f, "<<< zig rc=%d patched_resp=1\n", rc); fclose(f); }
        }
        if (rc != 0) goto spawn_failed;
        return rc;
    }

    final[0] = args[0];
    final[1] = args[1];
    for (int i = 2; i < n; i++) final[i] = args[i];
    final[n] = NULL;
    rc = _spawnv(_P_WAIT, ZIG_PATH, (const char *const *)final);
    if (getenv("ZIG_CC_LOG")) {
        FILE *f = fopen(getenv("ZIG_CC_LOG"), "a");
        if (f) { fprintf(f, "<<< zig rc=%d reemit=0\n", rc); fclose(f); }
    }
spawn_failed:
    /* spawn 失败时把 errno 带出来：rc=-1 说明进程压根没起来（路径 / 命令行 /
     * 环境块太长之类）。不打印的话 rustc 那边只剩一句 exit code: 0xffffffff。 */
    if (rc != 0) {
        DWORD e = GetLastError();
        fprintf(stderr, "[zig-cc-wrapper] spawn failed rc=%d errno=%d winerr=%lu\n",
                rc, errno, (unsigned long)e);
        if (getenv("ZIG_CC_LOG")) {
            FILE *f = fopen(getenv("ZIG_CC_LOG"), "a");
            if (f) { fprintf(f, "<<< spawn failed rc=%d errno=%d winerr=%lu\n", rc, errno, (unsigned long)e); fclose(f); }
        }
    }
    return rc;
}
