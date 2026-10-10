#!/usr/bin/env python3
"""Dogfood worker simulator for the self-hosting driver.

Watches <work_root>/outbox for dispatch envelopes, simulates the worker,
and drops `<digest>.receipt.json` files into <work_root>/inbox.

Modes:
  honest: write the task's expected output file into its workspace, then
          drop a pass-claim receipt.
  lying:  drop a pass-claim receipt WITHOUT doing the work (replays the
          run-1 operator error: shape-valid receipt, empty workspace).
  dirty:  do the honest work AND plant stray files (RT2 probe replay):
          a stray file in the workspace (outside allowed_scope) and one
          at the run_dir root. The scope check must fail the gate.
  launder: like dirty, but the worker COMMITS its strays
          (git add -A && git commit) before dropping the receipt —
          replays DS1-001/DS2-001/DS3-NEW-001. The baseline-pinned
          diff must still fail the gate.

Usage: seeder.py <work_root> <honest|lying|dirty|launder>
Stops when <work_root>/DRIVER-DONE exists.
"""
import json
import os
import subprocess
import sys
import time

WORK = {
    "DOG-A": ("one.txt", "one"),
    "DOG-B": ("two.txt", "two"),
    "DOG-C": ("three.txt", "three"),
}


def main():
    work_root, mode = sys.argv[1], sys.argv[2]
    assert mode in ("honest", "lying", "dirty", "launder"), "mode must be honest|lying|dirty|launder"
    outbox = os.path.join(work_root, "outbox")
    inbox = os.path.join(work_root, "inbox")
    workspaces = os.path.join(work_root, "workspaces")
    done = set()
    while not os.path.exists(os.path.join(work_root, "DRIVER-DONE")):
        if os.path.isdir(outbox):
            for name in sorted(os.listdir(outbox)):
                if not name.endswith(".json") or name.endswith(".receipt.json"):
                    continue
                digest = name[: -len(".json")]
                if digest in done:
                    continue
                with open(os.path.join(outbox, name)) as f:
                    env = json.load(f)
                task_id = env["contract"]["id"]
                ws_id = env["workspace"]
                ws_path = os.path.join(workspaces, ws_id)
                if mode in ("honest", "dirty", "launder") and task_id in WORK:
                    fname, content = WORK[task_id]
                    # Workspace is a git worktree; wait for it to appear.
                    for _ in range(100):
                        if os.path.isdir(ws_path):
                            break
                        time.sleep(0.1)
                    with open(os.path.join(ws_path, fname), "w") as f:
                        f.write(content + "\n")
                    print(f"[{mode}] {task_id}: wrote {fname} in {ws_id}", flush=True)
                    if mode in ("dirty", "launder"):
                        # RT2 probe: stray file in the workspace (outside
                        # allowed_scope) plus one at the run_dir root.
                        with open(os.path.join(ws_path, "stray.txt"), "w") as f:
                            f.write("stray\n")
                        with open(os.path.join(work_root, "root-stray.txt"), "w") as f:
                            f.write("stray\n")
                        print(f"[{mode}] {task_id}: planted strays", flush=True)
                    if mode == "launder":
                        # DS1-001 replay: commit the strays so `git status`
                        # looks clean. The baseline-pinned diff must catch it.
                        subprocess.run(["git", "-C", ws_path, "add", "-A"],
                                       check=True, capture_output=True)
                        subprocess.run(
                            ["git", "-C", ws_path, "-c", "user.email=w@x",
                             "-c", "user.name=w", "commit", "-qm", "work"],
                            capture_output=True)
                        print(f"[launder] {task_id}: committed strays", flush=True)
                else:
                    print(f"[{mode}] {task_id}: no work, receipt only", flush=True)
                receipt = {"envelope_digest": digest, "verdict": "pass"}
                rp = os.path.join(inbox, f"{digest}.receipt.json")
                tmp = rp + ".tmp"
                with open(tmp, "w") as f:
                    json.dump(receipt, f)
                os.rename(tmp, rp)
                done.add(digest)
        time.sleep(0.2)


if __name__ == "__main__":
    main()
