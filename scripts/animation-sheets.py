"""Builds one contact sheet per event from an animation-check recording.

Each sheet is the frames from just before an event (events.txt) onward, labelled with
their time since it. Used by .github/workflows/animation-check.yaml.

Usage: python animation-sheets.py <out dir> <dest dir> [frames per event]
"""
import os
import sys

from PIL import Image, ImageDraw

src, dest = sys.argv[1], sys.argv[2]
per_event = int(sys.argv[3]) if len(sys.argv) > 3 else 16
os.makedirs(dest, exist_ok=True)

frames = sorted(
    (int(name[:-4]), os.path.join(src, "frames", name))
    for name in os.listdir(os.path.join(src, "frames"))
    if name.endswith(".jpg")
)
events = []
with open(os.path.join(src, "events.txt"), encoding="utf-8-sig") as f:
    for line in f:
        ms, text = line.rstrip("\n").split("\t", 1)
        events.append((int(ms), text))

cols, thumb_w = 4, 480
for index, (ms, text) in enumerate(events):
    if text in ("start glazewm", "glazewm ready", "done"):
        continue
    picked = [fr for fr in frames if fr[0] >= ms - 100][:per_event]
    if not picked:
        continue
    first = Image.open(picked[0][1])
    thumb_h = int(first.height * thumb_w / first.width)
    rows = (len(picked) + cols - 1) // cols
    sheet = Image.new("RGB", (cols * thumb_w, rows * (thumb_h + 22)), "white")
    draw = ImageDraw.Draw(sheet)
    for i, (frame_ms, path) in enumerate(picked):
        x, y = (i % cols) * thumb_w, (i // cols) * (thumb_h + 22)
        sheet.paste(Image.open(path).resize((thumb_w, thumb_h)), (x, y + 22))
        draw.text((x + 4, y + 4), f"{text}  +{frame_ms - ms} ms", fill="black")
    name = f"{index:02d}-{text.replace(' ', '-')}.jpg"
    sheet.save(os.path.join(dest, name), quality=80)
    print(name, len(picked), "frames")
