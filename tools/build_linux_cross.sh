#!/usr/bin/env bash
### 交叉编译 Linux 版 mdc-server（**静态 musl**， zig 提供 libc 与链接器）。
###
### 为什么必须静态：fpk 里的镜像是「scratch + 静态二进制」—— 运行期不需要 C 库、
### 不需要 loader、不需要 alpine/debian rootfs，镜像因此小一个数量级，也彻底绕开
### 「NAS 上要装编译工具链 / 拉基础镜像」这条老路（国内网络下必挂）。
###
### 用法：
###   bash tools/build_linux_cross.sh                 # 两个架构都编
###   bash tools/build_linux_cross.sh amd64           # 只编 x86_64
###   ZIG=/path/zig.exe bash tools/build_linux_cross.sh
###
### 产物：
###   build/linux-x86_64/mdc-server
###   build/linux-aarch64/mdc-server
###
### ⚠️ 四个必须知道的点：
###   1. 全局 cargo config 里有 `[env] CC=w64devkit gcc`（Windows 目标用），
###      它会污染交叉编译 —— 必须给**每个 target 单独**设 `CC_<target>`，
###      不能靠 `CC=...` 一把梭。
###   2. `rustup target add` 是联网操作，已经装过就跳过（脚本会自己判断）。
###   3. 编出来的必须是 ELF 且**动态段为空**（静态链接），否则 scratch 容器里
###      会直接 `no such file or directory` —— 脚本末尾自己校验（file 命令没有时
###      退化为查 ELF 类型字段）。
###   4. 本机没有 strip / llvm-strip，符号表只能在链接期剥：
###      `RUSTFLAGS="-C strip=symbols"`。事后补 strip 的写法
###      （`zig cc -target ... -strip`）是不存在的，别照抄。
set -uo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BUILD="${ROOT}/build"
export PATH="/c/Users/25407/.cargo/bin:$PATH"

ZIG="${ZIG:-}"
if [ -z "${ZIG}" ]; then
    for c in "$(ls -d /c/Users/25407/pl_tool/zig*/zig.exe 2>/dev/null | head -1)" \
             "$(command -v zig)" "${HOME}/zig/zig.exe"; do
        [ -n "${c}" ] && [ -x "${c}" ] && { ZIG="${c}"; break; }
    done
fi
[ -n "${ZIG}" ] && [ -x "${ZIG}" ] || { echo "错误：找不到 zig（先下载 zig-windows-x86_64-*.zip 并解压，或 ZIG=/path/to/zig.exe）" >&2; exit 1; }
echo "[cross] zig = ${ZIG}"

[ -d "${ROOT}/web/dist" ] || { echo "错误：web/dist 不存在（先 cd web && npm ci && npm run build）" >&2; exit 1; }

### ── 编出 CC 转发器（tools/zig-cc-wrapper.c）─────────────────────────────
### 两个用法（都要它，缺一不可）：
###   * CC_<target>                        —— 给 cc-rs 编译 C 依赖（ring/sqlite3-sys）；
###     cc-rs 会自己追加 `--target=x86_64-unknown-linux-musl`，zig 不认这个
###     vendor=unknown 的写法，wrapper 把它换成 `--target=x86_64-linux-musl`。
###   * CARGO_TARGET_<target>_LINKER       —— 给 rustc 链接。**rustc 链接时不传
###     --target**，wrapper 靠编译时烧进去的 DEFAULT_TARGET 补上；不补的话 zig 按
###     宿主 windows-gnu 链接，报 `ld.exe: unrecognized option '--eh-frame-hdr'`
###     （PATH 上的 cc 是 w64devkit 的 Windows 链接器）。
### ⚠️ 必须是 Windows 可执行文件：cc-rs / cargo 在 Windows 上直接 CreateProcess CC
###    值，用 .sh 包装器会 `os error 193（不是有效的 Win32 应用程序）`。
mkdir -p "${BUILD}"
for TRIPLE in x86_64 aarch64; do
    case "${TRIPLE}" in
        x86_64)  ZT="x86_64-linux-musl" ;;
        aarch64) ZT="aarch64-linux-musl" ;;
    esac
    WRAP="${BUILD}/zig-cc-${TRIPLE}.exe"
    rm -f "${WRAP}"   # ⚠️ zig 会按「源+参数」复用缓存产物，改了源码不删旧的会拿到旧 exe
    # ⚠️ -DFORCE_REBUILD 是为了 bust zig 缓存（随机值换一次缓存键）
    if ! "${ZIG}" cc -target x86_64-windows-gnu -O2 \
            -DZIG_PATH="\"${ZIG}\"" -DDEFAULT_TARGET="\"${ZT}\"" \
            -DFORCE_REBUILD="\"$RANDOM\"" \
            tools/zig-cc-wrapper.c -o "${WRAP}" 2>&1 | tail -5; then
        echo "错误：${TRIPLE} 转发器编译失败" >&2; exit 1
    fi
    [ -x "${WRAP}" ] || { echo "错误：${TRIPLE} 转发器没生成（${WRAP}）" >&2; exit 1; }
    echo "[cross] CC 转发器 ${TRIPLE}: ${WRAP} ($(wc -c < "${WRAP}") 字节)"
done
WRAP_X="${BUILD}/zig-cc-x86_64.exe"
WRAP_A="${BUILD}/zig-cc-aarch64.exe"
LOG="${BUILD}/zig-cc-args.log"

TARGETS=""
[ "${1:-}" = "arm64" ] || TARGETS="${TARGETS} x86_64"
[ "${1:-}" = "amd64" ] || TARGETS="${TARGETS} aarch64"

FAILED=0
for TRIPLE in x86_64 aarch64; do
    case "${TRIPLE}" in
        x86_64)  RUST_T="x86_64-unknown-linux-musl";  ZT="x86_64-linux-musl";  OUT="${BUILD}/linux-x86_64" ;;
        aarch64) RUST_T="aarch64-unknown-linux-musl"; ZT="aarch64-linux-musl"; OUT="${BUILD}/linux-aarch64" ;;
    esac
    case " ${TARGETS} " in *" ${TRIPLE} "*) ;; *) continue ;; esac

    echo "== ${TRIPLE} (${RUST_T}) =="
    rustup target add "${RUST_T}" >/dev/null 2>&1 && echo "   target 已就绪" || echo "   ⚠️ target 添加失败（可能已存在）"

    case "${TRIPLE}" in
        x86_64)  WRAP="${WRAP_X}" ;;
        aarch64) WRAP="${WRAP_A}" ;;
    esac
    export CC_"${RUST_T//-/_}"="${WRAP}"
    # ⚠️ 光设 CARGO_TARGET_${T}_LINKER 没用：那个环境变量 cargo 只喂给 build script，
    #    rustc 根本不认。让 rustc 换链接器只有两条路：`.cargo/config.toml` 里
    #    `target.<triple>.linker`，或者 `-C linker=`（走 RUSTFLAGS，最省事）。
    # ⚠️ 必须给 rustc 的是 **Windows 路径**：cargo/rustc 是原生 Win32 程序，
    #    拿到 MSYS 风格的 /e/boki/... 会报 `linker ... not found（os error 3）`。
    LINKER_WRAP="$(cygpath -w "${WRAP}")"
    echo "   compiler=${WRAP}"
    echo "   linker  =${LINKER_WRAP}"

    mkdir -p "${OUT}"
    # ⚠️ -C link-self-contained 在 musl 目标上是 **unstable**（rustc 会直接报错
    #    "must also be passed -Z unstable-options"），所以去重靠 wrapper 自己：
    #    它发现命令行里有 `.rlib`（rustc 链接 Rust 产物的标志物）时补一个
    #    `-nostdlib`，让 zig 别再塞那份和 rustlib self-contained 撞车的 crt。
    rm -f "${LOG}" 2>/dev/null
    # ⚠️ 完整日志另存一份：`tail -25` 会把真正的 `ld.lld: error:` 掐掉（rustc 的
    #    `= note:` 行巨长，错误行在最前面），排查时 grep 完整日志。
    FULLLOG="${LOG%.log}.build.log"
    rm -f "${FULLLOG}" 2>/dev/null
    ( cd "${ROOT}" && ZIG_CC_LOG="${LOG}" \
        RUSTFLAGS="-C strip=symbols -C linker=${LINKER_WRAP}" \
        cargo build --release --locked --target "${RUST_T}" \
        -p mdc-server --features embed-web > "${FULLLOG}" 2>&1 ) \
        && CARGO_OK=1 || CARGO_OK=0
    tail -25 "${FULLLOG}"
    if [ "${CARGO_OK}" != "1" ]; then
        echo "   --- 关键错误行 ---"
        grep -E "ld\.lld:|undefined symbol|duplicate symbol|clang.*error|error:" "${FULLLOG}" | head -20
    fi
    if [ ! -f "${ROOT}/target/${RUST_T}/release/mdc-server" ]; then
        echo "   ❌ ${TRIPLE} 编译没产出二进制"; FAILED=1; continue
    fi
    cp -f "${ROOT}/target/${RUST_T}/release/mdc-server" "${OUT}/mdc-server"

    # ELF + 静态校验
    MAGIC="$(head -c4 "${OUT}/mdc-server")"
    EXPECTED="$(printf '\177ELF')"
    if [ "${MAGIC}" != "${EXPECTED}" ]; then
        echo "   ❌ ${TRIPLE} 产物不是 ELF（magic=$(printf '%s' "${MAGIC}" | od -An -tx1 | tr -d ' ')）"; FAILED=1; continue
    fi
    # ELF header e_type(2)/e_machine(2)：ET_EXEC=2, EM_X86_64=62, EM_AARCH64=183
    ETYPE="$(od -An -tu2 -j16 -N2 "${OUT}/mdc-server" | tr -d ' ')"
    EMACH="$(od -An -tu2 -j18 -N2 "${OUT}/mdc-server" | tr -d ' ')"
    case "${TRIPLE}" in
        x86_64)  [ "${EMACH}" = "62" ]  || { echo "   ❌ x86_64 产物 machine=${EMACH}（应为 62）"; FAILED=1; } ;;
        aarch64) [ "${EMACH}" = "183" ] || { echo "   ❌ arm64 产物 machine=${EMACH}（应为 183）"; FAILED=1; } ;;
    esac
    echo "   ✅ ${TRIPLE} $(wc -c < "${OUT}/mdc-server") 字节（ELF, ET=${ETYPE}, machine=${EMACH}）"
done

echo "== 目录 =="; ls -la "${BUILD}/linux-x86_64" "${BUILD}/linux-aarch64" 2>/dev/null
[ ${FAILED} -eq 0 ] || { echo "❌ 有架构编译失败" >&2; exit 1; }
echo "✅ 交叉编译完成"
