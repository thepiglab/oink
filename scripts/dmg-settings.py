# dmgbuild settings for Oink's disk image: the app on the left and an Applications link on the right,
# over the arrow and first-launch hint drawn by dmg-background.js (600 x 380 points).
# scripts/bundle-macos.sh runs:
#   dmgbuild -s scripts/dmg-settings.py -D app=dist/Oink.app -D background=… Oink out.dmg
import os.path

app = defines["app"]
files = [app]
symlinks = {"Applications": "/Applications"}
icon = os.path.join(app, "Contents", "Resources", "AppIcon.icns")
background = defines["background"]
format = "ULMO"

window_rect = ((200, 200), (600, 380))
icon_size = 128
text_size = 13
icon_locations = {os.path.basename(app): (150, 170), "Applications": (450, 170)}
