#!/bin/bash
# Verify the character-selection (charlist) portrait on the REAL GL client:
# boots a fresh server + Xvfb + the real client through run-real-client-e2e.sh
# machinery, waits for the character card through the real widget chain
# (DriveAgent), captures the selection screen, reads the AVATAR FRAME region
# (the 74x74 card area, measured from the real layout) and prints a
# PORTRAIT: OK|FAIL verdict. Screenshots stay at /tmp/client_charlist.png and
# /tmp/avatar_back.png for human review.
#
# Verdict rule: the avatar frame must contain the composited character -
# a solid block of dark pixels (body/hair sprites) on the light card
# background. An empty frame shows only the card pattern (~0 dark pixels).
set -u
TAG="${1:-portrait}"
REPO="${REPO:-$(cd "$(dirname "$0")/../.." && pwd)}"

bash $REPO/scripts/jogl/run-real-client-e2e.sh driveuser36 $TAG >/tmp/portrait_run_$TAG.log 2>&1

rg -q "CHARLIST SCREENSHOT: saved" /tmp/client_$TAG.log 2>/dev/null || {
  echo "PORTRAIT: FAIL (no charlist screenshot; client log follows)"; tail -20 /tmp/client_$TAG.log; exit 1; }
rg -q "AVATAR COMPOSITE: images=[1-9]" /tmp/client_$TAG.log || {
  echo "PORTRAIT: FAIL (avatar composite empty)"; exit 1; }

python3 - <<'EOF'
from PIL import Image
img = Image.open('/tmp/client_charlist.png').convert('RGB')
# Avatar frame inside the charlist card on the 1024x768 screen
# (charlist at center+(-380,-50); the avatar frame occupies the left part
# of the first card; region measured from the real client layout).
crop = img.crop((148, 350, 214, 422))
dark = sum(1 for p in crop.getdata() if (p[0] + p[1] + p[2]) / 3 < 95)
print(f'PORTRAIT REGION: {dark} dark pixels')
if dark >= 400:
    print('PORTRAIT: OK')
    raise SystemExit(0)
else:
    print('PORTRAIT: FAIL (avatar frame is empty - no composited character)')
    raise SystemExit(1)
EOF
