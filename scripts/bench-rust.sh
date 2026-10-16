#!/usr/bin/env bash
set -euo pipefail

summarize() {
  awk -F'\t' '
    NR == 1 { next }
    { key = $1 SUBSEP $3; if (!(key in n)) order[++k] = key; n[key]++
      for (c = 4; c <= 11; c++) v[key, c, n[key]] = $c
      v[key, 12, n[key]] = $5 > 0 ? $6 / 1000 / $5 : 0 }
    function med(key, c,   m, i, j, t, a) {
      m = n[key]; for (i = 1; i <= m; i++) a[i] = v[key, c, i]
      for (i = 2; i <= m; i++) { t = a[i]; for (j = i - 1; j >= 1 && a[j] + 0 > t + 0; j--) a[j + 1] = a[j]; a[j + 1] = t }
      return m % 2 ? a[(m + 1) / 2] : (a[m / 2] + a[m / 2 + 1]) / 2
    }
    END {
      print "| scenario | step | runs | wall s | CPU s | avg cores | memory.peak GB | anon peak GB | target/ GB | sccache GB | load1 |"
      print "|---|---|---|---|---|---|---|---|---|---|---|"
      for (i = 1; i <= k; i++) {
        key = order[i]; split(key, p, SUBSEP)
        printf "| %s | %s | %d | %.1f | %.1f | %.1f | %.2f | %.2f | %.2f | %.2f | %s |\n", p[1], p[2], n[key],
          med(key, 5) / 1000, med(key, 6) / 1e6, med(key, 12), med(key, 7) / 1e9, med(key, 8) / 1e9,
          med(key, 9) / 1e9, med(key, 10) / 1e9, med(key, 11)
      }
    }' "$1/steps.tsv" >"$1/summary.md"
}

usage() {
  cat >&2 <<'USAGE'
usage: buildq scripts/bench-rust.sh [--setup CMD] [--runs N] <src-dir> <out-dir>
       scripts/bench-rust.sh --summarize <out-dir>
Benchmarks cold, warm-cache, warm-incremental and no-op Rust builds and tests of <src-dir>, a plain
copy (git archive), not a checkout; run it on an idle machine. --setup runs in <src-dir> once before
the cold scenario. Writes <out-dir>/steps.tsv (one row per step) and <out-dir>/summary.md (medians;
avg cores = CPU time / wall time). --summarize rewrites summary.md from steps.tsv.
USAGE
  exit 2
}
die() { echo "bench: $*" >&2; exit 1; }
setup='' runs=3
while [[ ${1:-} == --* ]]; do
  case $1 in
    --setup) setup=${2:?}; shift 2 ;;
    --runs) runs=${2:?}; shift 2 ;;
    --summarize) summarize "${2:?}"; cat "$2/summary.md"; exit ;;
    *) usage ;;
  esac
done
(($# == 2)) || usage
for tool in systemd-run make cargo; do command -v "$tool" >/dev/null || die "$tool not found"; done

main=$(git -C "$(dirname "$0")" worktree list --porcelain | sed -n '1s/^worktree //p')
[[ -f $main/.env ]] || die "no $main/.env; run 'make dev-infra' in the main checkout"
src=$(realpath "$1")
out=$(realpath -m "$2")
[[ -f $src/Cargo.toml ]] || die "$src is not the workspace root"
mkdir -p "$out/log"
[[ ! -e $out/steps.tsv ]] || die "$out/steps.tsv exists; use a new out dir"

unset RUSTC_WRAPPER CARGO_INCREMENTAL CARGO_TARGET_DIR CARGO_BUILD_RUSTC_WRAPPER CARGO_BUILD_INCREMENTAL
export CARGO_BUILD_JOBS=6 CARGO_TERM_COLOR=never
export SCCACHE_DIR=$out/sccache SCCACHE_SERVER_PORT=4299

cd "$src"
ln -sfn "$main/.env" .env

# sccache compiles in its server's process tree, so the server starts and stops inside each
# step's scope; otherwise compile CPU would land in whichever step started it.
# shellcheck disable=SC2016
measure='
cg=/sys/fs/cgroup$(sed -n "s/^0:://p" /proc/self/cgroup)
log=$1; shift
peak_file=$(mktemp)
echo 0 >"$peak_file"
(peak=0; while :; do
  a=$(awk "\$1==\"anon\" {print \$2}" "$cg/memory.stat"); ((a > peak)) && peak=$a; echo "$peak" >"$peak_file"
  sleep 1
done) &
sampler=$!
has_sccache=$(command -v sccache || true)
t0=$(date +%s%N)
[[ -z $has_sccache ]] || sccache --start-server >>"$log" 2>&1
rc=0; "$@" >>"$log" 2>&1 || rc=$?
[[ -z $has_sccache ]] || { sccache --show-stats >>"$log" 2>&1; sccache --stop-server >/dev/null 2>&1 || true; }
t1=$(date +%s%N)
kill "$sampler"; wait "$sampler" 2>/dev/null || true
printf "%s\t%s\t%s\t%s\t%s\n" "$rc" $(((t1 - t0) / 1000000)) \
  "$(awk "\$1==\"usage_usec\" {print \$2}" "$cg/cpu.stat")" "$(cat "$cg/memory.peak")" "$(cat "$peak_file")"
rm -f "$peak_file"'

size() { [[ -e $1 ]] && du -sb "$1" | cut -f1 || echo 0; }

printf 'scenario\trun\tstep\trc\twall_ms\tcpu_us\tmem_peak_b\tanon_peak_b\ttarget_b\tsccache_b\tload1\n' >"$out/steps.tsv"
failed=0
step() {
  local scenario=$1 run=$2 name=$3 load res
  shift 3
  load=$(cut -d' ' -f1 /proc/loadavg)
  echo "bench: $scenario #$run $name (load $load)" >&2
  res=$(systemd-run --user --scope --quiet --collect -- bash -c "$measure" _ "$out/log/$scenario-$run-$name.log" "$@")
  [[ ${res%%$'\t'*} == 0 ]] || { failed=1; echo "bench: $name failed, see $out/log/$scenario-$run-$name.log" >&2; }
  printf '%s\t%s\t%s\t%s\t%s\t%s\t%s\n' "$scenario" "$run" "$name" "$res" "$(size target)" "$(size "$SCCACHE_DIR")" "$load" >>"$out/steps.tsv"
}

build=(cargo build --workspace --locked)
test_compile=(cargo test --workspace --locked --no-run)
test_run=(make test-rust)
clippy=(cargo clippy --workspace --all-targets --locked -- -D warnings)

sccache --stop-server >/dev/null 2>&1 || true
rm -rf target "$SCCACHE_DIR"
[[ -z $setup ]] || bash -c "$setup"

step cold 1 build "${build[@]}"
step cold 1 test-compile "${test_compile[@]}"
step cold 1 test-run "${test_run[@]}"
step cold 1 clippy "${clippy[@]}"

for ((i = 1; i <= runs; i++)); do
  rm -rf target
  step warm-cache "$i" build "${build[@]}"
  step warm-cache "$i" test-compile "${test_compile[@]}"
done

for ((i = 1; i <= runs; i++)); do
  echo "// bench edit $i" >>crates/commerce/src/lib.rs
  step warm-incremental "$i" test-compile "${test_compile[@]}"
  step warm-incremental "$i" test-run "${test_run[@]}"
done

for ((i = 1; i <= runs; i++)); do
  step no-op "$i" test-run "${test_run[@]}"
done

summarize "$out"
cat "$out/summary.md"
exit "$failed"
