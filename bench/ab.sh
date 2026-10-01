#!/bin/bash
# Interleaved A/B scan benchmark: ./bench/ab.sh <binary A> <binary B> <rounds> <path>
# Alternates A and B (flipping order each round) so both see the same background load.
set -euo pipefail
A=$1; B=$2; ROUNDS=${3:-8}; DIR=${4:-$HOME}
ta=(); tb=()
run() { "$1" --bench-scan "$DIR" 1 2>&1 | awk '/^run 0:/ {print $3, $4, $5, $6}'; }
for i in $(seq 1 "$ROUNDS"); do
  if (( i % 2 )); then ra=$(run "$A"); rb=$(run "$B"); else rb=$(run "$B"); ra=$(run "$A"); fi
  ta+=("${ra%%s *}"); tb+=("${rb%%s *}")
  # Files/nodes must match exactly; bytes may drift by up to 1 MB on a live tree (TOLERANCE=0 for exact).
  read -r fa ba na <<<"$(echo "${ra#* }" | tr '=' ' ' | awk '{print $2, $4, $6}')"
  read -r fb bb nb <<<"$(echo "${rb#* }" | tr '=' ' ' | awk '{print $2, $4, $6}')"
  drift=$(( ba > bb ? ba - bb : bb - ba ))
  # COUNT_TOLERANCE: allowed file/node count drift (0 = exact); only for live trees like /.
  dfiles=$(( fa > fb ? fa - fb : fb - fa )); dnodes=$(( na > nb ? na - nb : nb - na ))
  if [[ $dfiles -gt ${COUNT_TOLERANCE:-0} || $dnodes -gt ${COUNT_TOLERANCE:-0} || $drift -gt ${TOLERANCE:-1000000} ]]; then
    echo "FINGERPRINT MISMATCH: A=${ra#* } B=${rb#* }"; exit 1
  fi
done
fp="${ra#* }"
median() { printf '%s\n' "$@" | sort -n | awk '{a[NR]=$1} END {print (NR%2 ? a[(NR+1)/2] : (a[NR/2]+a[NR/2+1])/2)}'; }
wins=0; for i in "${!ta[@]}"; do awk -v a="${ta[$i]}" -v b="${tb[$i]}" 'BEGIN{exit !(b<a)}' && wins=$((wins+1)); done
ma=$(median "${ta[@]}"); mb=$(median "${tb[@]}")
echo "A median ${ma}s | B median ${mb}s | B/A $(awk -v a="$ma" -v b="$mb" 'BEGIN{printf "%.3f", b/a}') | B faster in $wins/$ROUNDS pairs"
echo "fingerprint (both): $fp"
