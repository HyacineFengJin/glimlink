#!/bin/sh
# Development-only byte check of the fixed public compatibility constant.
set -eu
umask 077
reference_revision=${1:-master}
case "$reference_revision" in
  master) ;;
  *[!0-9a-f]*|'') echo 'Use master or an exact lowercase commit hash.' >&2; exit 2 ;;
esac
curl --fail --silent --show-error --max-time 20 --proto '=https' \
  "https://raw.githubusercontent.com/eatmoreapple/openwechat/$reference_revision/global.go" \
  -o /cache/weixin-reference-global.go
awk '
  /uosPatchExtspam[[:space:]]*=/ { active=1 }
  active && match($0, /"[^"]*"/) {
    printf "%s", substr($0, RSTART+1, RLENGTH-2)
    if ($0 !~ /\+[[:space:]]*$/) exit
  }
' /cache/weixin-reference-global.go > /cache/weixin-reference-header.txt
awk '
  /const EXTSPAM/ { active=1; next }
  active && /\);/ { exit }
  active && match($0, /"[^"]*"/) { printf "%s", substr($0, RSTART+1, RLENGTH-2) }
' /workspace/crates/platform-weixin/src/desktop.rs > /cache/weixin-rust-header.txt
cmp -s /cache/weixin-reference-header.txt /cache/weixin-rust-header.txt
printf '{"reference_matched":true,"header_bytes":%s}\n' "$(wc -c < /cache/weixin-rust-header.txt)"
