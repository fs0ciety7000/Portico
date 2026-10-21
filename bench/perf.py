#!/usr/bin/env python3
"""Portico performance harness (docs/PERF.md). Measures, never modifies the product.

Requirements: Linux, python3 + `pyte` (TUI screen emulation) and `python-xlib` (GUI),
OpenSSH `sshd`/`ssh-keygen` (sessions), Xvfb (GUI). Release binaries:
  cargo build --release -p portico-tui
  (cd apps/portico-gui && pnpm build) && cargo build --release -p portico-gui --features tauri/custom-protocol

Everything runs in a throwaway root (HOME, XDG_*, ~/.ssh, sshd): the real ~/.ssh is never read.

  python3 bench/perf.py tui-start  --bin target/release/portico --runs 20 [--hosts 50] [--drop-caches]
  python3 bench/perf.py tui-idle   --bin target/release/portico --seconds 30 --sessions 0|1|5 [--strace]
  python3 bench/perf.py tui-load   --bin target/release/portico --hosts 2000 --includes 8 --runs 10
  python3 bench/perf.py gui-start  --bin target/release/portico-gui --runs 10 [--drop-caches]
  python3 bench/perf.py gui-idle   --bin target/release/portico-gui --seconds 30 --sessions 0|5

Each command prints a JSON summary on stdout (progress on stderr).
"""
from __future__ import annotations

import argparse
import fcntl
import json
import os
import pty
import select
import shutil
import signal
import statistics
import struct
import subprocess
import sys
import tempfile
import termios
import time
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
import fixtures  # noqa: E402

CLK_TCK = os.sysconf("SC_CLK_TCK")
EXTRA_ENV: list[str] = []
COLS, ROWS = 120, 40


def log(*a: object) -> None:
    print(*a, file=sys.stderr, flush=True)


def now() -> float:
    return time.perf_counter()


def pctl(xs: list[float], q: float) -> float:
    """Nearest-rank percentile (q in 0..100)."""
    import math
    s = sorted(xs)
    k = max(0, min(len(s) - 1, math.ceil(q / 100 * len(s)) - 1))
    return s[k]


def summary(xs: list[float]) -> dict:
    if not xs:
        return {}
    return {"n": len(xs), "median": round(statistics.median(xs), 2), "p95": round(pctl(xs, 95), 2),
            "min": round(min(xs), 2), "max": round(max(xs), 2), "runs": [round(x, 1) for x in xs]}


def drop_caches() -> bool:
    try:
        os.sync()
        Path("/proc/sys/vm/drop_caches").write_text("3\n")
        return True
    except OSError:
        return False


# ---------------------------------------------------------------- /proc sampling

def cpu_ticks(pid: int) -> int:
    """utime + stime of the whole process (all threads), in clock ticks."""
    data = Path(f"/proc/{pid}/stat").read_text()
    fields = data[data.rindex(")") + 2:].split()
    return int(fields[11]) + int(fields[12])


def children(pid: int) -> list[int]:
    out = []
    for task in Path(f"/proc/{pid}/task").iterdir():
        try:
            out += [int(c) for c in (task / "children").read_text().split()]
        except OSError:
            pass
    return out


def tree(pid: int) -> list[int]:
    todo, seen = [pid], []
    while todo:
        p = todo.pop()
        if p in seen:
            continue
        seen.append(p)
        try:
            todo += children(p)
        except OSError:
            pass
    return seen


def comm(pid: int) -> str:
    try:
        return Path(f"/proc/{pid}/comm").read_text().strip()
    except OSError:
        return "?"


def mem(pid: int) -> dict:
    """RSS / PSS / private (USS) in KiB, from smaps_rollup."""
    out = {}
    try:
        for line in Path(f"/proc/{pid}/smaps_rollup").read_text().splitlines():
            parts = line.split()
            if parts and parts[0] in ("Rss:", "Pss:", "Private_Clean:", "Private_Dirty:"):
                out[parts[0][:-1]] = int(parts[1])
    except OSError:
        return {}
    return {"rss_kib": out.get("Rss", 0), "pss_kib": out.get("Pss", 0),
            "uss_kib": out.get("Private_Clean", 0) + out.get("Private_Dirty", 0)}


def tree_mem(pid: int) -> dict:
    procs = []
    for p in tree(pid):
        m = mem(p)
        if m:
            procs.append({"pid": p, "comm": comm(p), **m})
    total = {k: sum(p[k] for p in procs) for k in ("rss_kib", "pss_kib", "uss_kib")}
    return {"total": total, "procs": procs}


def tree_cpu(pid: int) -> dict[int, int]:
    out = {}
    for p in tree(pid):
        try:
            out[p] = cpu_ticks(p)
        except OSError:
            pass
    return out


# ---------------------------------------------------------------- environment

class Sandbox:
    """Throwaway root: HOME, XDG dirs, ~/.ssh (fixtures.py) and optionally a local sshd."""

    def __init__(self, hosts: int, includes: int, with_sshd: bool, alive: int = 60) -> None:
        self.root = Path(tempfile.mkdtemp(prefix="portico-perf-"))
        self.sshd: subprocess.Popen | None = None
        port = hostkey = None
        if with_sshd:
            port, hostkey = self._start_sshd()
        fixtures.write(self.root, hosts, includes, port, hostkey, alive)
        if with_sshd:
            pub = (self.root / "home/.ssh/id_ed25519.pub").read_text()
            (self.root / "sshd/authorized_keys").write_text(pub)
        self.expected_hosts = hosts + 2 + (5 if with_sshd else 0)

    def _start_sshd(self) -> tuple[int, str]:
        d = self.root / "sshd"
        (d / "home").mkdir(parents=True)
        subprocess.run(["ssh-keygen", "-q", "-t", "ed25519", "-N", "", "-f", str(d / "host_ed25519")],
                       check=True)
        import socket
        s = socket.socket()
        s.bind(("127.0.0.1", 0))
        port = s.getsockname()[1]
        s.close()
        (d / "authorized_keys").write_text("")
        (d / "sshd_config").write_text(
            f"Port {port}\nListenAddress 127.0.0.1\nHostKey {d}/host_ed25519\nPidFile {d}/sshd.pid\n"
            f"AuthorizedKeysFile {d}/authorized_keys\nSetEnv HOME={d}/home\n"
            "StrictModes no\nUsePAM no\nPasswordAuthentication no\nKbdInteractiveAuthentication no\n"
            "PermitRootLogin yes\nPrintMotd no\nPrintLastLog no\nLogLevel ERROR\n")
        Path("/run/sshd").mkdir(exist_ok=True)
        self.sshd = subprocess.Popen(["/usr/sbin/sshd", "-D", "-e", "-f", str(d / "sshd_config")],
                                     stdout=subprocess.DEVNULL, stderr=open(d / "sshd.log", "w"))
        for _ in range(100):
            try:
                socket.create_connection(("127.0.0.1", port), timeout=0.2).close()
                break
            except OSError:
                time.sleep(0.05)
        return port, str(d / "host_ed25519.pub")

    def env(self) -> dict:
        r = self.root
        return {
            "PATH": "/usr/bin:/bin:/usr/sbin:/sbin",
            "TERM": "xterm-256color",
            "LANG": "C.UTF-8",
            "HOME": str(r / "home"),
            "USER": os.environ.get("USER", "root"),
            "XDG_CONFIG_HOME": str(r / "xdg/config"),
            "XDG_DATA_HOME": str(r / "xdg/local/share"),
            "XDG_STATE_HOME": str(r / "xdg/local/state"),
            "XDG_CACHE_HOME": str(r / "xdg/cache"),
            # No agent: authentication uses the throwaway IdentityFile only.
        }

    def close(self) -> None:
        if self.sshd:
            self.sshd.terminate()
            self.sshd.wait()
        shutil.rmtree(self.root, ignore_errors=True)


# ---------------------------------------------------------------- TUI driver

class Tui:
    def __init__(self, binary: str, env: dict, prefix: list[str] | None = None) -> None:
        import pyte
        self.screen = pyte.Screen(COLS, ROWS)
        self.stream = pyte.ByteStream(self.screen)
        master, slave = pty.openpty()
        fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", ROWS, COLS, 0, 0))
        self.master = master
        self.bytes_read = 0
        self.reads = 0
        self.t0 = now()
        self.proc = subprocess.Popen((prefix or []) + [binary], env=env, stdin=slave, stdout=slave,
                                     stderr=slave, start_new_session=True, close_fds=True,
                                     preexec_fn=lambda: fcntl.ioctl(0, termios.TIOCSCTTY, 0))
        os.close(slave)
        self.pid = self.proc.pid

    def text(self) -> str:
        return "\n".join(self.screen.display)

    def pump(self, timeout: float) -> float | None:
        """Read what is available within `timeout`; returns arrival time of the last chunk."""
        r, _, _ = select.select([self.master], [], [], timeout)
        if not r:
            return None
        try:
            data = os.read(self.master, 65536)
        except OSError:
            raise EOFError
        if not data:
            raise EOFError
        t = now()
        self.bytes_read += len(data)
        self.reads += 1
        self.stream.feed(data)
        return t

    def wait_for(self, pred, timeout: float = 10.0) -> float:
        """Seconds since spawn at which pred(screen text) first held."""
        deadline = now() + timeout
        if pred(self.text()):
            return now() - self.t0
        while now() < deadline:
            t = self.pump(max(0.0, deadline - now()))
            if t is not None and pred(self.text()):
                return t - self.t0
        raise TimeoutError(f"condition not met; screen:\n{self.text()}")

    def record(self, quiet: float = 0.5, limit: float = 30.0) -> list[tuple[float, bytes]]:
        """Read raw output, timestamped on arrival, without parsing it (pyte is slow and would
        delay the next read), until the program has been silent for `quiet` seconds."""
        chunks: list[tuple[float, bytes]] = []
        end = now() + limit
        while now() < end:
            r, _, _ = select.select([self.master], [], [], quiet)
            if not r:
                if chunks:
                    break
                continue
            data = os.read(self.master, 65536)
            if not data:
                break
            chunks.append((now(), data))
        return chunks

    def first_match(self, chunks: list[tuple[float, bytes]], preds: list) -> list[float | None]:
        """Replays recorded chunks into the screen; seconds since spawn at which each predicate
        first held (arrival time of the chunk that completed it)."""
        found: list[float | None] = [None] * len(preds)
        for t, data in chunks:
            self.bytes_read += len(data)
            self.reads += 1
            self.stream.feed(data)
            text = self.text()
            for i, p in enumerate(preds):
                if found[i] is None and p(text):
                    found[i] = t - self.t0
        return found

    def drain(self, seconds: float) -> None:
        end = now() + seconds
        while now() < end:
            try:
                self.pump(max(0.0, end - now()))
            except EOFError:
                return

    def send(self, data: bytes) -> None:
        os.write(self.master, data)

    def quit(self) -> None:
        try:
            self.send(b"\x03")  # Ctrl-C quits from the host list
            self.proc.wait(timeout=3)
        except Exception:
            self.proc.kill()
            self.proc.wait()
        finally:
            os.close(self.master)


def list_ready(n: int):
    """Host list fully drawn: header counter, last list row, status bar."""
    needle = f"Hosts {n}"

    def pred(text: str) -> bool:
        lines = text.split("\n")
        return (needle in lines[0] and "●" in lines[ROWS - 3] and "quit" in lines[ROWS - 1])
    return pred


def first_frame(text: str) -> bool:
    return "PORTICO" in text


def cmd_tui_start(a) -> dict:
    sb = Sandbox(a.hosts, a.includes, with_sshd=False)
    first, ready, dropped = [], [], None
    try:
        for i in range(a.runs + a.warmup):
            if a.drop_caches:
                dropped = drop_caches()
            t = Tui(a.bin, sb.env())
            f, r = t.first_match(t.record(), [first_frame, list_ready(sb.expected_hosts)])
            t.quit()
            if f is None or r is None:
                raise RuntimeError(f"list not drawn:\n{t.text()}")
            if i >= a.warmup:
                first.append(f * 1000)
                ready.append(r * 1000)
            log(f"run {i}: first frame {f*1000:.1f} ms, list {r*1000:.1f} ms")
            time.sleep(0.2)
    finally:
        sb.close()
    return {"hosts": sb.expected_hosts, "drop_caches": dropped, "first_frame_ms": summary(first),
            "list_ready_ms": summary(ready)}


def shell_prompt(text: str) -> bool:
    """A remote shell prompt (`...$` or `...#`) on a body line of the terminal tab."""
    import re
    lines = text.split("\n")[1:-1]
    return any(re.search(r"[$#] ?$", l.rstrip() + " ") and l.strip() for l in lines)


def connect(t: Tui, alias: str) -> float:
    """Search an alias and connect; returns seconds until the remote prompt shows."""
    t.send(b"/")
    t.drain(0.15)
    t.send(alias.encode())
    t.drain(0.15)
    t0 = now()
    t.send(b"\r")
    t.wait_for(shell_prompt, timeout=15)
    return now() - t0


def go_home(t: Tui) -> None:
    t.send(b"\x00")  # Ctrl-Space prefix
    t.drain(0.1)
    t.send(b"h")
    t.drain(0.3)
    t.send(b"\x1b")  # leave search field / clear
    t.drain(0.2)


def cmd_tui_idle(a) -> dict:
    sb = Sandbox(a.hosts, a.includes, with_sshd=a.sessions > 0, alive=a.alive)
    strace_out = sb.root / "strace.txt"
    res: dict = {"sessions": a.sessions, "seconds": a.seconds, "strace": a.strace}
    try:
        t = Tui(a.bin, sb.env())
        t.wait_for(list_ready(sb.expected_hosts))
        target = t.pid
        res["mem_list"] = mem(target)
        conn = []
        for k in range(1, a.sessions + 1):
            if k > 1:
                go_home(t)
            conn.append(connect(t, f"local-{k}") * 1000)
            t.drain(0.5)
        if conn:
            res["connect_to_prompt_ms"] = summary(conn)
        t.drain(2.0)  # let the last animation (status toast, spinner) settle
        res["view"] = "terminal tab" if a.sessions else "host list"
        tracer = None
        if a.strace:  # every syscall of every thread during the idle window only
            tracer = subprocess.Popen(["strace", "-f", "-qq", "-c", "-o", str(strace_out),
                                       "-p", str(target)], stderr=subprocess.DEVNULL)
            time.sleep(0.5)
        c0, b0, r0, w0 = cpu_ticks(target), t.bytes_read, t.reads, now()
        t.drain(a.seconds)
        c1, b1, r1, w1 = cpu_ticks(target), t.bytes_read, t.reads, now()
        if tracer:
            tracer.send_signal(signal.SIGINT)
            tracer.wait()
            res["strace_idle"] = strace_out.read_text()
        res["idle"] = {"cpu_ticks": c1 - c0, "cpu_ms": (c1 - c0) * 1000 / CLK_TCK,
                       "cpu_pct": round((c1 - c0) / CLK_TCK / (w1 - w0) * 100, 3),
                       "pty_bytes": b1 - b0, "pty_reads": r1 - r0, "wall_s": round(w1 - w0, 2)}
        res["mem_end"] = mem(target)
        res["threads"] = len(list(Path(f"/proc/{target}/task").iterdir()))
        if a.dump:
            log(t.text())
        t.quit()
    finally:
        sb.close()
    return res


def cmd_tui_load(a) -> dict:
    """Big config: is a frame drawn before the inventory arrives, and does input get handled?"""
    sb = Sandbox(a.hosts, a.includes, with_sshd=False)
    first, ready = [], []
    loading_seen = 0
    try:
        for i in range(a.runs):
            if a.drop_caches:
                drop_caches()
            t = Tui(a.bin, sb.env())
            loading = lambda s: "Reading ~/.ssh/config" in s  # noqa: E731
            f, ld, r = t.first_match(t.record(limit=max(30.0, a.seconds)),
                                     [first_frame, loading, list_ready(sb.expected_hosts)])
            t.quit()
            if f is None or r is None:
                raise RuntimeError(f"list not drawn:\n{t.text()}")
            if ld is not None:
                loading_seen += 1
            first.append(f * 1000)
            ready.append(r * 1000)
            log(f"run {i}: first frame {f*1000:.1f} ms, list {r*1000:.1f} ms")
            time.sleep(0.2)
        probe = input_probe(a, sb) if a.probe else None
    finally:
        sb.close()
    return {"hosts": sb.expected_hosts, "includes": a.includes, "first_frame_ms": summary(first),
            "list_ready_ms": summary(ready), "loading_placeholder_seen": f"{loading_seen}/{a.runs}",
            "input_during_load": probe}


THEMES = ("Obsidian", "Basalt", "Travertine")


def theme_of(text: str) -> str | None:
    head = text.split("\n")[0]
    return next((th for th in THEMES if th in head), None)


def input_probe(a, sb: Sandbox) -> list[dict]:
    """While the inventory is still loading, press `t` (next theme, shown in the header) and
    time how long the UI takes to show it, versus when the list arrives."""
    out = []
    for i in range(a.probe):
        t = Tui(a.bin, sb.env())
        t.wait_for(lambda s: "Reading ~/.ssh/config" in s)
        before = theme_of(t.text())
        time.sleep(0.2)  # well inside the load (seconds for thousands of hosts)
        sent = now()
        t.send(b"t")
        t.wait_for(lambda s: theme_of(s) not in (None, before), timeout=a.seconds)
        reacted = now() - sent
        still_loading = "Reading ~/.ssh/config" in t.text()
        t.wait_for(list_ready(sb.expected_hosts), timeout=max(30.0, a.seconds))
        loaded = now() - sent
        t.quit()
        out.append({"theme_change_ms": round(reacted * 1000, 1), "still_loading": still_loading,
                    "list_after_key_ms": round(loaded * 1000, 1)})
        log(f"probe {i}: {out[-1]}")
    return out


# ---------------------------------------------------------------- GUI driver

class Xvfb:
    def __init__(self, display: str = ":97", size: str = "1600x1000x24") -> None:
        self.display = display
        self.proc = subprocess.Popen(["Xvfb", display, "-screen", "0", size, "-nolisten", "tcp"],
                                     stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        from Xlib import display as xd
        for _ in range(100):
            try:
                self.d = xd.Display(display)
                return
            except Exception:
                time.sleep(0.05)
        raise RuntimeError("Xvfb did not start")

    def close(self) -> None:
        self.d.close()
        self.proc.terminate()
        self.proc.wait()


def gui_env(sb: Sandbox, display: str) -> dict:
    env = sb.env()
    env.update({"DISPLAY": display, "XDG_RUNTIME_DIR": str(sb.root / "run"),
                "NO_AT_BRIDGE": "1"})
    env.update(dict(kv.split("=", 1) for kv in (EXTRA_ENV or [])))
    (sb.root / "run").mkdir(mode=0o700, exist_ok=True)
    return env


def find_window(d, name: str):
    from Xlib import X
    root = d.screen().root

    def walk(w):
        try:
            attrs = w.get_attributes()
            if attrs.map_state == X.IsViewable:
                wn = w.get_wm_name()
                if wn == name:
                    return w
            for c in w.query_tree().children:
                r = walk(c)
                if r is not None:
                    return r
        except Exception:
            return None
        return None
    return walk(root)


def non_uniform(d, win, step: int = 3) -> tuple[bool, int]:
    """Has the header strip (logo, wordmark, Hosts tab: 640x44 at the top left) anything but
    the window background? Small region so that one poll costs ~2 ms (full window: ~25 ms)."""
    from Xlib import X
    img = win.get_image(0, 0, 640, 44, X.ZPixmap, 0xFFFFFFFF)
    data = img.data
    seen = set()
    for o in range(0, len(data), 4 * step):
        seen.add(bytes(data[o:o + 3]))
        if len(seen) > 8:
            return True, len(seen)
    return False, len(seen)


def sidebar_text(d, win) -> int:
    """Bright ("marble" text) pixels in the host-list area of the sidebar. The loading skeleton
    has none; real host rows have alias text. Window-relative box for the default 1280x800 window."""
    from Xlib import X
    img = win.get_image(16, 130, 300, 300, X.ZPixmap, 0xFFFFFFFF)
    data = img.data
    bright = 0
    for o in range(0, len(data), 4 * 3):  # every third pixel
        b, g, r = data[o], data[o + 1], data[o + 2]
        if (r + g + b) > 3 * 150:
            bright += 1
    return bright


def cmd_gui_start(a) -> dict:
    sb = Sandbox(a.hosts, a.includes, with_sshd=False)
    x = Xvfb()
    mapped, painted, content, listed, dropped = [], [], [], [], None
    try:
        for i in range(a.runs + a.warmup):
            if a.drop_caches:
                dropped = drop_caches()
            t0 = now()
            p = subprocess.Popen([a.bin], env=gui_env(sb, x.display), stdout=subprocess.DEVNULL,
                                 stderr=open(sb.root / f"gui-{i}.log", "w"), start_new_session=True)
            win = None
            tm = tp = tc = tl = None
            deadline = t0 + 30
            while now() < deadline:
                if win is None:
                    win = find_window(x.d, "Portico")
                    if win is not None:
                        tm = now() - t0
                if win is not None:
                    try:
                        ok, colours = non_uniform(x.d, win)
                    except Exception:
                        ok, colours = False, 0
                    if tp is None and colours > 1:
                        tp = now() - t0  # first paint different from blank
                    if ok and tc is None:
                        tc = now() - t0  # real content (many colours: text, sidebar)
                    if tc is not None and sidebar_text(x.d, win) > 20:
                        tl = now() - t0  # host rows drawn (the skeleton has no bright text)
                        break
                time.sleep(0.005)
            if a.shot and i == a.runs + a.warmup - 1:
                subprocess.run(["import", "-display", x.display, "-window", "root", a.shot])
            os.killpg(p.pid, signal.SIGTERM)
            try:
                p.wait(timeout=5)
            except subprocess.TimeoutExpired:
                os.killpg(p.pid, signal.SIGKILL)
                p.wait()
            log(f"run {i}: mapped {tm}, first paint {tp}, content {tc}, host list {tl}")
            if i >= a.warmup and tm is not None:
                mapped.append(tm * 1000)
                if tp is not None:
                    painted.append(tp * 1000)
                if tc is not None:
                    content.append(tc * 1000)
                if tl is not None:
                    listed.append(tl * 1000)
            time.sleep(0.5)
    finally:
        x.close()
        sb.close()
    return {"drop_caches": dropped, "window_mapped_ms": summary(mapped),
            "first_paint_ms": summary(painted), "content_ms": summary(content),
            "host_list_ms": summary(listed), "extra_env": a.env}


def xdotool(display: str, *args: str) -> None:
    subprocess.run(["xdotool", *args], env={**os.environ, "DISPLAY": display}, check=False)


def established_to(port: int) -> int:
    """ESTABLISHED TCP connections whose remote port is `port` (/proc/net/tcp, no `ss` needed)."""
    n = 0
    for line in Path("/proc/net/tcp").read_text().splitlines()[1:]:
        f = line.split()
        if f[3] == "01" and int(f[2].split(":")[1], 16) == port:
            n += 1
    return n


def cmd_gui_idle(a) -> dict:
    sb = Sandbox(a.hosts, a.includes, with_sshd=a.sessions > 0, alive=a.alive)
    x = Xvfb()
    res: dict = {"sessions": a.sessions, "seconds": a.seconds}
    try:
        p = subprocess.Popen([a.bin], env=gui_env(sb, x.display), stdout=subprocess.DEVNULL,
                             stderr=open(sb.root / "gui.log", "w"), start_new_session=True)
        win = None
        for _ in range(600):
            win = find_window(x.d, "Portico")
            if win is not None and non_uniform(x.d, win)[0]:
                break
            time.sleep(0.05)
        time.sleep(3)
        res["mem_list"] = tree_mem(p.pid)
        wid = str(win.id)
        xdotool(x.display, "windowfocus", "--sync", wid)
        for k in range(1, a.sessions + 1):
            # Alt-0: host list; "/": search field; select its text, type the alias; Enter connects.
            for step in (["key", "alt+0"], ["key", "slash"], ["key", "ctrl+a"],
                         ["type", "--delay", "20", f"local-{k}"], ["key", "Return"]):
                xdotool(x.display, *step)
                time.sleep(0.4)
            time.sleep(3)
        if a.sessions and a.home_after:  # sessions stay open, the host list is shown
            xdotool(x.display, "key", "alt+0")
        res["view"] = "host list" if (not a.sessions or a.home_after) else "terminal tab"
        if a.shot:
            subprocess.run(["import", "-display", x.display, "-window", "root", a.shot])
        time.sleep(3)
        c0, w0 = tree_cpu(p.pid), now()
        time.sleep(a.seconds)
        c1, w1 = tree_cpu(p.pid), now()
        per = {}
        for pid, t1 in c1.items():
            d = t1 - c0.get(pid, 0)
            per[f"{comm(pid)}:{pid}"] = round(d * 1000 / CLK_TCK, 1)
        total = sum(c1.values()) - sum(v for k, v in c0.items() if k in c1)
        res["idle"] = {"cpu_ms_by_proc": per, "cpu_ms": total * 1000 / CLK_TCK,
                       "cpu_pct": round(total / CLK_TCK / (w1 - w0) * 100, 3), "wall_s": round(w1 - w0, 2)}
        res["mem_end"] = tree_mem(p.pid)
        # Established TCP connections to the throwaway sshd: proof the sessions exist.
        port = (sb.root / "home/.ssh/config").read_text().split("Port ")[1].split()[0] if a.sessions else None
        if port:
            res["ssh_connections"] = established_to(int(port))
        os.killpg(p.pid, signal.SIGTERM)
        p.wait(timeout=5)
    finally:
        x.close()
        sb.close()
    return res


def main() -> None:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = ap.add_subparsers(dest="cmd", required=True)
    for name in ("tui-start", "tui-idle", "tui-load", "gui-start", "gui-idle"):
        s = sub.add_parser(name)
        s.add_argument("--bin", required=True)
        s.add_argument("--hosts", type=int, default=50)
        s.add_argument("--includes", type=int, default=3)
        s.add_argument("--runs", type=int, default=20)
        s.add_argument("--warmup", type=int, default=0)
        s.add_argument("--seconds", type=float, default=30)
        s.add_argument("--sessions", type=int, default=0)
        s.add_argument("--drop-caches", action="store_true")
        s.add_argument("--strace", action="store_true")
        s.add_argument("--dump", action="store_true")
        s.add_argument("--shot")
        s.add_argument("--home-after", action="store_true", help="gui-idle: back to the host list")
        s.add_argument("--env", action="append", default=[], help="GUI: extra KEY=VALUE")
        s.add_argument("--probe", type=int, default=0, help="tui-load: key presses during the load")
        s.add_argument("--alive", type=int, default=60, help="ServerAliveInterval (keepalive/latency probe)")
    a = ap.parse_args()
    EXTRA_ENV.extend(a.env)
    fn = {"tui-start": cmd_tui_start, "tui-idle": cmd_tui_idle, "tui-load": cmd_tui_load,
          "gui-start": cmd_gui_start, "gui-idle": cmd_gui_idle}[a.cmd]
    print(json.dumps(fn(a), indent=2))


if __name__ == "__main__":
    main()
