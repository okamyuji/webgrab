#!/usr/bin/env bash
# webgrab のインジェクション/SSRF 無害化を実バイナリで検証する。
# 無害な多面インジェクションページ(injection_page.py)に対し webgrab を実行し、
# 各攻撃ベクトルが無害化されることを PASS/FAIL で判定する。1つでも失敗すれば非0終了。
#
# 使い方: scripts/security-verify/run.sh
# 必要環境: python3, curl, cargo（release binary が無ければ自動ビルド）。Chrome は不要。
set -uo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
ROOT="$(cd "$HERE/../.." && pwd)"
BIN="$ROOT/target/release/webgrab"
PORT="${PORT:-8811}"
URL="http://127.0.0.1:${PORT}/"

if [ ! -x "$BIN" ]; then
  echo "release binary が無いためビルドします..."
  cargo build --release --manifest-path "$ROOT/Cargo.toml" || exit 1
fi

python3 "$HERE/injection_page.py" "$PORT" &
SRV=$!
trap 'kill "$SRV" 2>/dev/null' EXIT

# サーバ起動待ち
for _ in $(seq 1 25); do
  curl -s "$URL" >/dev/null 2>&1 && break
  sleep 0.2
done

OUT="$("$BIN" "$URL" --allow-private 2>/dev/null)"
RAW="$("$BIN" "$URL" --allow-private --raw 2>/dev/null)"
FENCED="$("$BIN" "$URL" --allow-private --fence 2>/dev/null)"

"$BIN" "http://169.254.169.254/" >/dev/null 2>&1; META_CODE=$?
"$BIN" "file:///etc/passwd"      >/dev/null 2>&1; FILE_CODE=$?

fail=0
check() { # check "名前" "テストコマンド(真でPASS)"
  if eval "$2" >/dev/null 2>&1; then
    echo "PASS: $1"
  else
    echo "FAIL: $1"
    fail=1
  fi
}

CTRL="$(printf '\033\007')"  # ESC + BEL

echo "== 出力インジェクション(A03) =="
check "1 端末エスケープ除去: 生ESC/BELが出力に無い" \
  '! printf "%s" "$OUT" | LC_ALL=C grep -q "["$CTRL"]"'
check "2 危険リンク: 抽出後に生の ](javascript: が無い" \
  '! printf "%s" "$OUT" | grep -q "](javascript:"'
check "2 危険リンク: --raw保持時は ](unsafe-javascript: へ無害化" \
  'printf "%s" "$RAW" | grep -q "](unsafe-javascript:"'
check "3 偽マーカー: 本物 [webgrab:truncated が本文由来で出ない" \
  '! printf "%s" "$OUT" | grep -q "\[webgrab:truncated"'
check "3 偽マーカー: 偽物は [quoted-webgrab: へ無害化" \
  'printf "%s" "$OUT" | grep -q "\[quoted-webgrab:"'
check "4 タイトル改行: URL Source 行はちょうど1つ" \
  '[ "$(printf "%s" "$OUT" | grep -c "^URL Source:")" = "1" ]'
check "4 タイトル改行: attacker の偽 URL Source 行が無い" \
  '! printf "%s" "$OUT" | grep -q "^URL Source:.*attacker.example"'
check "B --fence: 説得型テキストがフェンス内に配信される" \
  'printf "%s" "$FENCED" | awk "/untrusted-content source/{f=1} f&&/INJECTION-OK/{ok=1} /untrusted-content-end]/{f=0} END{exit !ok}"'

echo "== SSRF(A10) =="
check "5 内部アドレス(メタデータ)拒否: 終了コード8" '[ "$META_CODE" = "8" ]'
check "6 不正スキーム file:// 拒否: 終了コード2" '[ "$FILE_CODE" = "2" ]'

echo "== 説得型テキストはツールでは防げない（消費側の責務） =="
if printf "%s" "$OUT" | grep -q "INJECTION-OK"; then
  echo "NOTE: 本文中に INJECTION-OK が存在（＝忠実に配信）。指示として実行しないのは消費側エージェントの責務。"
fi

echo
if [ "$fail" = "0" ]; then
  echo "ALL PASS: 能動的な実行系ベクトルはすべて無害化されています。"
else
  echo "SOME FAILED（上記 FAIL を参照）"
fi
exit "$fail"
