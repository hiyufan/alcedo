"""Record the alcedo CLI into an animated SVG (docs/demo.svg).

Runs real commands in a pseudo-terminal, types them character by character,
samples the screen with pyte while they run, and stitches the screens into a
CSS-animated SVG. Each terminal row is drawn once per stretch of time it stays
the same, so typing does not repeat the whole screen in every frame.

Waits longer than MAX_WAIT seconds (network round trips) are shortened to
MAX_WAIT; everything else plays at the speed it ran.

    cargo build --release -p alcedo-cli
    uv run --with pyte python3 scripts/record-demo.py docs/demo.svg
"""
import fcntl
import os
import pty
import select
import struct
import sys
import tempfile
import termios
import time
from html import escape

import pyte

COLS, ROWS = 100, 12
SCENES = [  # (command, seconds to keep the result on screen)
    ('alcedo download "7.99 复制打开抖音，看看【作者】的作品 https://v.douyin.com/pe2jeHe8nDs/ 很好看"', 2.0),
    ("alcedo download --format 1080p https://www.youtube.com/watch?v=dQw4w9WgXcQ", 4.0),
]
TYPE_CHARS, TYPE_DELAY = 3, 0.05  # characters per keystroke frame, seconds between them
MAX_WAIT = 2.0
TIMEOUT = 120

repo = os.path.join(os.path.dirname(os.path.abspath(__file__)), "..")
out_path = sys.argv[1] if len(sys.argv) > 1 else os.path.join(repo, "docs", "demo.svg")
workdir = tempfile.mkdtemp(prefix="alcedo-demo-")

env = {
    "PATH": os.path.join(repo, "target", "release") + os.pathsep + os.environ["PATH"],
    "HOME": workdir,
    "TERM": "xterm-256color",
    "LANG": "C.UTF-8",
    "COLUMNS": str(COLS),
    "LINES": str(ROWS),
    # 绿色的 $ 提示符；\[ \] 让 readline 不把颜色码算进宽度
    "PS1": "\\[\\e[32m\\]$\\[\\e[0m\\] ",
}
pid, fd = pty.fork()
if pid == 0:
    os.chdir(workdir)
    os.execvpe("bash", ["bash", "--norc", "--noprofile"], env)
fcntl.ioctl(fd, termios.TIOCSWINSZ, struct.pack("HHHH", ROWS, COLS, 0, 0))
screen = pyte.Screen(COLS, ROWS)
stream = pyte.ByteStream(screen)


def pump(seconds):
    end = time.time() + seconds
    while True:
        left = end - time.time()
        if left <= 0:
            return
        r, _, _ = select.select([fd], [], [], min(left, 0.05))
        if r:
            try:
                stream.feed(os.read(fd, 65536))
            except OSError:
                return


def snapshot():
    rows = tuple(
        tuple((c.data, c.fg, c.bold) for c in (screen.buffer[y][x] for x in range(COLS)))
        for y in range(ROWS)
    )
    return rows, (screen.cursor.x, screen.cursor.y)


def at_prompt():
    line = screen.display[screen.cursor.y].rstrip()
    return line == "$"


frames = []  # [(rows, cursor, seconds)]


def keep(seconds):
    shot = snapshot()
    if frames and frames[-1][0] == shot[0] and frames[-1][1] == shot[1]:
        rows, cursor, held = frames[-1]
        frames[-1] = (rows, cursor, held + seconds)
    else:
        frames.append((*shot, seconds))


pump(1.0)
for command, hold in SCENES:
    keep(0.6)
    for i in range(0, len(command), TYPE_CHARS):
        os.write(fd, command[i : i + TYPE_CHARS].encode())
        pump(TYPE_DELAY)
        keep(TYPE_DELAY)
    pump(0.3)
    keep(0.3)
    os.write(fd, b"\r")
    started = last = time.time()
    while time.time() - started < TIMEOUT:
        pump(0.1)
        now = time.time()
        shot = snapshot()
        if shot[0] != frames[-1][0] or shot[1] != frames[-1][1]:
            # 上一帧在屏幕上停了多久就记多久，网络等待压到 MAX_WAIT
            rows, cursor, held = frames[-1]
            frames[-1] = (rows, cursor, held + min(now - last, MAX_WAIT))
            frames.append((*shot, 0.0))
            last = now
        if at_prompt() and now - last > 0.3:
            break
    else:
        sys.exit(f"命令超过 {TIMEOUT} 秒没结束: {command}")
    rows, cursor, held = frames[-1]
    frames[-1] = (rows, cursor, held + hold)
os.write(fd, b"exit\r")
pump(0.3)

# ---------------------------------------------------------------- SVG
PALETTE = {
    "default": "#e6e6e6",
    "green": "#5af78e",
    "red": "#ff6b6b",
    "yellow": "#f3f99d",
    "brown": "#f3f99d",
    "cyan": "#57c7ff",
    "blue": "#57c7ff",
    "magenta": "#ff6ac1",
}
CW, LH, PAD, TOP = 8.4, 19, 18, 40
W, H = int(COLS * CW + PAD * 2), int(ROWS * LH + TOP + PAD)
total = sum(f[2] for f in frames)


def wide(ch):
    return ord(ch[:1] or " ") > 0x2E7F


def row_svg(cells, y):
    parts, x = [], 0
    while x < COLS:
        data, fg, bold = cells[x]
        x2 = x
        # 宽字符（中文、emoji）各占一个元素，不管用什么字体都落在终端的格子上
        while x2 < COLS and cells[x2][1:] == (fg, bold) and wide(cells[x2][0]) == wide(data):
            x2 += 1
            if wide(data):
                x2 += 1  # pyte 在宽字符后面留一个空格子
                break
        text = "".join(cells[i][0] for i in range(x, min(x2, COLS)))
        if text.strip():
            weight = ' font-weight="bold"' if bold else ""
            color = PALETTE.get(fg, PALETTE["default"])
            parts.append(
                f'<text x="{PAD + x * CW:.1f}" y="{y}" fill="{color}"{weight} xml:space="preserve">{escape(text)}</text>'
            )
        x = x2
    return "".join(parts)


# 每一行（和光标）按"内容不变的一段时间"出一个元素，同样时间窗口的共用一条动画
windows, elements = {}, []


def place(svg, start, end):
    key = (round(start / total * 100, 3), round(end / total * 100, 3))
    cls = windows.setdefault(key, f"w{len(windows)}")
    elements.append(f'<g class="{cls}">{svg}</g>')


for y in range(ROWS):
    t, start, current = 0.0, 0.0, None
    for rows, _, held in frames:
        if rows[y] != current:
            if current is not None and "".join(c[0] for c in current).strip():
                place(row_svg(current, TOP + 14 + y * LH), start, t)
            current, start = rows[y], t
        t += held
    if current is not None and "".join(c[0] for c in current).strip():
        place(row_svg(current, TOP + 14 + y * LH), start, t)

t, start, current = 0.0, 0.0, None
for _, cursor, held in frames:
    if cursor != current:
        if current is not None:
            cx, cy = current
            place(f'<rect x="{PAD + cx * CW:.1f}" y="{TOP + cy * LH}" width="{CW:.1f}" height="{LH - 2}" fill="#e6e6e6" opacity="0.7"/>', start, t)
        current, start = cursor, t
    t += held
cx, cy = current
place(f'<rect x="{PAD + cx * CW:.1f}" y="{TOP + cy * LH}" width="{CW:.1f}" height="{LH - 2}" fill="#e6e6e6" opacity="0.7"/>', start, t)

css = []
for (a, b), cls in windows.items():
    if a <= 0 and b >= 100:
        continue  # 一直在屏幕上的不用动画
    css.append(
        f"@keyframes {cls}{{0%,{a}%{{visibility:hidden}}{a}%,{b}%{{visibility:visible}}{b}%,100%{{visibility:hidden}}}}"
        f".{cls}{{visibility:hidden;animation:{cls} {total:.2f}s steps(1) infinite}}"
    )

svg = f'''<svg xmlns="http://www.w3.org/2000/svg" width="{W}" height="{H}" viewBox="0 0 {W} {H}" font-family="ui-monospace,SFMono-Regular,Menlo,Consolas,'Noto Sans Mono CJK SC',monospace" font-size="14">
<style>{"".join(css)}@media (prefers-reduced-motion:reduce){{g{{animation-play-state:paused}}}}</style>
<rect width="{W}" height="{H}" rx="10" fill="#1e1f29"/>
<circle cx="22" cy="20" r="6" fill="#ff5f57"/><circle cx="42" cy="20" r="6" fill="#febc2e"/><circle cx="62" cy="20" r="6" fill="#28c840"/>
<text x="{W / 2}" y="25" fill="#8b8d98" text-anchor="middle" font-size="13">alcedo</text>
{"".join(elements)}
</svg>'''
os.makedirs(os.path.dirname(os.path.abspath(out_path)), exist_ok=True)
with open(out_path, "w", encoding="utf-8") as f:
    f.write(svg)
print(f"{len(frames)} frames, {len(elements)} elements, {total:.1f}s, {len(svg) // 1024} KB -> {out_path}")
