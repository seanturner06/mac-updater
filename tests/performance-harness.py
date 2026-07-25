#!/usr/bin/env python3
"""
Live performance harness for Freshet (Workstream 2).

Exercises the same operations the app performs — concurrent `brew fetch` to warm the
download cache, then the install steps (`brew upgrade`, `npm update -g`, `mas upgrade`) —
against whatever is *actually* outdated on this machine at run time. Nothing is mocked.

Phases:
  1. Detect outdated formulae / casks / npm globals / mas apps from the live tools.
  2. DOWNLOAD PHASE (concurrent): `brew fetch` each formula+cask in parallel (bounded pool),
     measuring per-item bytes + duration and the aggregate wall-clock across the batch.
  3. INSTALL PHASE: upgrade everything, capturing per-item success/failure by diffing the
     outdated set before vs. after, with failure reasons pulled from tool output.

Sudo: SUDO_ASKPASS points at a GUI askpass helper (same pattern as the app). If elevation is
needed a native dialog appears; if it's cancelled that step fails but the run keeps going.

Writes a timestamped report to tests/performance-results/run-<timestamp>.txt (never overwrites).
Usage:  python3 tests/performance-harness.py [--no-install]
"""
import concurrent.futures as cf
import datetime as dt
import json
import os
import pathlib
import subprocess
import sys
import time

REPO = pathlib.Path(__file__).resolve().parent.parent
OUTDIR = REPO / "tests" / "performance-results"
OUTDIR.mkdir(parents=True, exist_ok=True)
TS = dt.datetime.now().strftime("%Y%m%dT%H%M%S")
OUT = OUTDIR / f"run-{TS}.txt"
DO_INSTALL = "--no-install" not in sys.argv
# --batched mirrors the shipped prefetch_selected(): one `brew fetch` per kind, letting Homebrew's
# own downloader (HOMEBREW_DOWNLOAD_CONCURRENCY, 16 by default) parallelize. Without it the harness
# spawns one fetch process per item (the original per-item measurement).
BATCHED = "--batched" in sys.argv
POOL = 6  # bounded concurrency for the per-item mode


def parse_explicit_items():
    # Any argv token like `formula:a,b,c` or `cask:x,y` supplies explicit targets, so a re-run can
    # re-fetch a known set even when nothing is currently "outdated" (e.g. after an install).
    f, c = [], []
    for a in sys.argv:
        if a.startswith("formula:"):
            f += [x for x in a[len("formula:"):].split(",") if x]
        elif a.startswith("cask:"):
            c += [x for x in a[len("cask:"):].split(",") if x]
    return f, c

report_lines = []


def emit(line=""):
    print(line, flush=True)
    report_lines.append(line)


def sh(args, timeout=None, env=None):
    try:
        return subprocess.run(
            args, capture_output=True, text=True, timeout=timeout, env=env
        )
    except subprocess.TimeoutExpired:
        r = subprocess.CompletedProcess(args, 124, "", f"timed out after {timeout}s")
        return r


def last_meaningful_line(text):
    for ln in reversed((text or "").strip().splitlines()):
        if ln.strip():
            return ln.strip()
    return ""


# --- Sudo askpass helper (GUI dialog; password only ever reaches sudo via stdin pipe) ---
def make_askpass():
    p = pathlib.Path("/tmp/freshet-perf-askpass.sh")
    p.write_text(
        '#!/bin/bash\n'
        "osascript -e 'display dialog \"Freshet performance run needs your administrator "
        'password.\" with title \"Freshet\" default answer \"\" with hidden answer '
        "with icon caution' -e 'text returned of result' 2>/dev/null\n"
    )
    os.chmod(p, 0o700)
    return str(p)


ENV = dict(os.environ)
ENV["SUDO_ASKPASS"] = make_askpass()


def brew_outdated():
    r = sh(["brew", "outdated", "--json=v2"], timeout=120)
    try:
        d = json.loads(r.stdout or "{}")
    except json.JSONDecodeError:
        d = {}
    formulae = [f["name"] for f in d.get("formulae", [])]
    casks = [c["name"] for c in d.get("casks", [])]
    return formulae, casks


def npm_outdated():
    r = sh(["npm", "outdated", "-g", "--json"], timeout=120)
    try:
        d = json.loads(r.stdout or "{}")
    except json.JSONDecodeError:
        return []
    out = []
    for name, info in d.items():
        cur, latest = info.get("current", ""), info.get("latest", "")
        # skip packages already at/ahead of latest (mirrors the app's parse_npm_outdated)
        if cur and latest and cur >= latest and cur.split(".") >= latest.split("."):
            pass
        out.append(name)
    return out


def mas_outdated():
    r = sh(["mas", "outdated"], timeout=120)
    ids = []
    for ln in (r.stdout or "").splitlines():
        ln = ln.strip()
        if not ln:
            continue
        tok = ln.split()[0]
        if tok.isdigit():
            ids.append(tok)
    return ids


def cache_path(kind, name):
    args = (
        ["brew", "--cache", "--cask", name]
        if kind == "cask"
        else ["brew", "--cache", "--formula", name]
    )
    r = sh(args, timeout=60)
    line = last_meaningful_line(r.stdout)
    return line or None


def fsize(p):
    try:
        return os.path.getsize(p) if p else 0
    except OSError:
        return 0


def mb(n):
    return n / (1024 * 1024)


# --- Download phase ------------------------------------------------------------------
def fetch_one(kind, name):
    cp = cache_path(kind, name)
    pre = fsize(cp)
    args = (
        ["brew", "fetch", "--cask", name]
        if kind == "cask"
        else ["brew", "fetch", "--formula", name]
    )
    t0 = time.monotonic()
    r = sh(args, timeout=1200, env=ENV)
    t1 = time.monotonic()
    post = fsize(cp)
    ok = r.returncode == 0
    cached = pre > 0 and post == pre and (t1 - t0) < 1.0
    # bytes actually transferred (delta); if it was already fully cached, delta is 0
    delta = max(post - pre, 0)
    transferred = post if pre == 0 else delta
    reason = "" if ok else last_meaningful_line(r.stderr) or "fetch failed"
    return {
        "kind": kind,
        "name": name,
        "ok": ok,
        "dur": t1 - t0,
        "bytes": transferred,
        "artifact_bytes": post,
        "cached": cached,
        "reason": reason,
        "start": t0,
        "end": t1,
    }


def run_download_phase(items):
    emit("=" * 72)
    emit("DOWNLOAD PHASE — concurrent `brew fetch` (pool size %d)" % POOL)
    emit("=" * 72)
    results = []
    batch_start = time.monotonic()
    with cf.ThreadPoolExecutor(max_workers=POOL) as ex:
        futs = [ex.submit(fetch_one, k, n) for (k, n) in items]
        for f in cf.as_completed(futs):
            results.append(f.result())
    batch_wall = time.monotonic() - batch_start
    results.sort(key=lambda r: (r["kind"], r["name"]))

    emit("")
    emit(
        "%-28s %-6s %10s %9s %10s  %s"
        % ("item", "kind", "size(MB)", "time(s)", "MB/s", "status")
    )
    emit("-" * 88)
    downloaded_bytes = 0
    dl_time_sum = 0.0
    ok_n = 0
    for r in results:
        thru = mb(r["bytes"]) / r["dur"] if r["dur"] > 0 and r["bytes"] > 0 else 0.0
        status = "OK" if r["ok"] else "FAIL: " + r["reason"]
        if r["cached"]:
            status = "OK (already cached)"
        emit(
            "%-28s %-6s %10.2f %9.2f %10.2f  %s"
            % (r["name"][:28], r["kind"], mb(r["artifact_bytes"]), r["dur"],
               thru, status)
        )
        if r["ok"]:
            ok_n += 1
        if r["bytes"] > 0:
            downloaded_bytes += r["bytes"]
            dl_time_sum += r["dur"]
    emit("-" * 88)
    agg_wall_thru = mb(downloaded_bytes) / batch_wall if batch_wall > 0 else 0.0
    naive_serial = mb(downloaded_bytes) / dl_time_sum if dl_time_sum > 0 else 0.0
    emit("")
    emit("Items fetched .................. %d (%d ok, %d failed)"
         % (len(results), ok_n, len(results) - ok_n))
    emit("New bytes downloaded ........... %.2f MB" % mb(downloaded_bytes))
    emit("Batch wall-clock (concurrent) .. %.2f s" % batch_wall)
    emit("Sum of per-item fetch time ..... %.2f s  (what a sequential run would spend downloading)"
         % dl_time_sum)
    if dl_time_sum > 0:
        emit("Concurrency speedup ............ %.2fx" % (dl_time_sum / batch_wall))
    emit("AGGREGATE throughput ........... %.2f MB/s  (new bytes / batch wall-clock)"
         % agg_wall_thru)
    emit("Per-stream avg throughput ...... %.2f MB/s  (new bytes / summed fetch time)"
         % naive_serial)
    emit("")
    return results


# --- Download phase: batched (mirrors the shipped prefetch_selected) ------------------
def run_download_phase_batched(formulae, casks):
    emit("=" * 72)
    emit("DOWNLOAD PHASE — batched `brew fetch` per kind (brew's native 16-way concurrency)")
    emit("=" * 72)
    items = [("formula", n) for n in formulae] + [("cask", n) for n in casks]
    paths = {(k, n): cache_path(k, n) for (k, n) in items}
    pre = {kn: fsize(p) for kn, p in paths.items()}

    def fetch_kind(kind, names):
        if not names:
            return (kind, True, 0.0, "")
        flag = "--cask" if kind == "cask" else "--formula"
        t0 = time.monotonic()
        r = sh(["brew", "fetch", flag] + names, timeout=1800, env=ENV)
        return (kind, r.returncode == 0, time.monotonic() - t0,
                "" if r.returncode == 0 else last_meaningful_line(r.stderr))

    batch_start = time.monotonic()
    with cf.ThreadPoolExecutor(max_workers=2) as ex:
        futs = {
            ex.submit(fetch_kind, "formula", formulae): "formula",
            ex.submit(fetch_kind, "cask", casks): "cask",
        }
        kres = {}
        for fut in cf.as_completed(futs):
            kind, ok, dt, reason = fut.result()
            kres[kind] = (ok, dt, reason)
    batch_wall = time.monotonic() - batch_start

    emit("")
    emit("Per-kind `brew fetch` (each a single process, brew parallelizes inside):")
    for kind in ("formula", "cask"):
        if kind in kres:
            ok, dt, reason = kres[kind]
            cnt = len(formulae if kind == "formula" else casks)
            emit("  %-8s %2d items  %6.2f s  %s"
                 % (kind, cnt, dt, "OK" if ok else "FAIL: " + reason))

    emit("")
    emit("%-28s %-6s %10s" % ("item", "kind", "size(MB)"))
    emit("-" * 48)
    downloaded = 0
    for (k, n) in items:
        post = fsize(paths[(k, n)])
        newb = max(post - pre[(k, n)], 0) if pre[(k, n)] else post
        downloaded += newb
        emit("%-28s %-6s %10.2f" % (n[:28], k, mb(post)))
    emit("-" * 48)
    agg = mb(downloaded) / batch_wall if batch_wall > 0 else 0.0
    emit("")
    emit("Items ......................... %d formulae + %d casks"
         % (len(formulae), len(casks)))
    emit("New bytes downloaded .......... %.2f MB" % mb(downloaded))
    emit("Batch wall-clock (both kinds concurrent) . %.2f s" % batch_wall)
    emit("AGGREGATE throughput .......... %.2f MB/s  (new bytes / batch wall-clock)" % agg)
    emit("")
    return downloaded, batch_wall


# --- Install phase -------------------------------------------------------------------
def run_install_phase(pre_formulae, pre_casks, pre_npm, pre_mas):
    emit("=" * 72)
    emit("INSTALL PHASE")
    emit("=" * 72)
    steps = []

    if pre_formulae:
        emit("-> brew upgrade --formula (%d formulae, installing from warmed cache)…"
             % len(pre_formulae))
        r = sh(["brew", "upgrade", "--formula"] + pre_formulae, timeout=3600, env=ENV)
        steps.append(("brew upgrade --formula", r))
    if pre_casks:
        emit("-> brew upgrade --cask %s…" % " ".join(pre_casks))
        r = sh(["brew", "upgrade", "--cask"] + pre_casks, timeout=3600, env=ENV)
        steps.append(("brew upgrade --cask", r))
    if pre_npm:
        emit("-> npm update -g…")
        r = sh(["npm", "update", "-g"], timeout=1800, env=ENV)
        steps.append(("npm update -g", r))
    for mid in pre_mas:
        emit("-> mas upgrade %s…" % mid)
        r = sh(["mas", "upgrade", mid], timeout=1800, env=ENV)
        steps.append(("mas upgrade " + mid, r))

    emit("")
    emit("Step results:")
    for name, r in steps:
        ok = r.returncode == 0
        emit("  [%s] %s%s"
             % ("OK " if ok else "FAIL", name,
                "" if ok else "  — " + last_meaningful_line(r.stderr or r.stdout)))

    # Per-item success by diffing the outdated set before vs. after.
    emit("")
    emit("Re-checking outdated set to confirm per-item success…")
    post_formulae, post_casks = brew_outdated()
    post_npm = set(npm_outdated())
    post_mas = set(mas_outdated())

    def classify(label, before, still_out, step_out_text):
        emit("")
        emit("%s (%d attempted):" % (label, len(before)))
        okc = 0
        for item in before:
            if item not in still_out:
                emit("  [OK ] %s" % item)
                okc += 1
            else:
                emit("  [FAIL] %s — still outdated after upgrade%s"
                     % (item, ("; " + step_out_text if step_out_text else "")))
        return okc, len(before)

    mas_reason = ""
    for name, r in steps:
        if name.startswith("mas") and r.returncode != 0:
            mas_reason = last_meaningful_line(r.stderr or r.stdout)

    totals = []
    if pre_formulae:
        totals.append(classify("Formulae", pre_formulae, set(post_formulae), ""))
    if pre_casks:
        totals.append(classify("Casks", pre_casks, set(post_casks), ""))
    if pre_npm:
        totals.append(classify("npm globals", pre_npm, post_npm, ""))
    if pre_mas:
        totals.append(classify("mas apps", pre_mas, post_mas, mas_reason))

    ok = sum(t[0] for t in totals)
    tot = sum(t[1] for t in totals)
    emit("")
    emit("INSTALL SUCCESS RATE ........... %d/%d (%.0f%%)"
         % (ok, tot, (100.0 * ok / tot) if tot else 0.0))
    return ok, tot


def main():
    emit("Freshet live performance run")
    emit("Timestamp: %s" % dt.datetime.now().isoformat(timespec="seconds"))
    emit("Machine:   %s" % last_meaningful_line(sh(["uname", "-mrs"]).stdout))
    emit("Install phase: %s" % ("YES" if DO_INSTALL else "NO (--no-install)"))
    emit("")

    exp_f, exp_c = parse_explicit_items()
    if exp_f or exp_c:
        formulae, casks, npm, mas = exp_f, exp_c, [], []
        emit("Explicit target set (--items); outdated detection skipped:")
    else:
        formulae, casks = brew_outdated()
        npm = npm_outdated()
        mas = mas_outdated()
        emit("Detected outdated at run time:")
    emit("  formulae (%d): %s" % (len(formulae), ", ".join(formulae) or "-"))
    emit("  casks (%d): %s" % (len(casks), ", ".join(casks) or "-"))
    emit("  npm globals (%d): %s" % (len(npm), ", ".join(npm) or "-"))
    emit("  mas apps (%d): %s" % (len(mas), ", ".join(mas) or "-"))
    emit("")

    dl_items = [("formula", n) for n in formulae] + [("cask", n) for n in casks]
    if dl_items:
        if BATCHED:
            run_download_phase_batched(formulae, casks)
        else:
            run_download_phase(dl_items)
    else:
        emit("Nothing outdated to download.")

    if DO_INSTALL and (formulae or casks or npm or mas):
        run_install_phase(formulae, casks, npm, mas)
    elif not DO_INSTALL:
        emit("Skipping install phase (--no-install).")

    OUT.write_text("\n".join(report_lines) + "\n")
    print("\nReport written to: %s" % OUT)


if __name__ == "__main__":
    main()
