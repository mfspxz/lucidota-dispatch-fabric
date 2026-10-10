#!/usr/bin/env python3
"""Dogfood worker simulator for the self-hosting driver.

Watches <work_root>/outbox for dispatch envelopes, simulates the worker,
and drops `<digest>.receipt.json` files into <work_root>/inbox.

Modes:
  honest: write the task's expected output file into its workspace, then
          drop a pass-claim receipt.
  lying:  drop a pass-claim receipt WITHOUT doing the work (replays the
          run-1 operator error: shape-valid receipt, empty workspace).

Usage: seeder.py <work_root> <honest|lying>
Stops when <work_root>/DRIVER-DONE exists.
"""
import json
import os
import sys
import time

WORK = {
    "DOG-A": ("one.txt", "one"),
    "DOG-B": ("two.txt", "two"),
    "DOG-C": ("three.txt", "three"),
}


def main():
    work_root, mode = sys.argv[1], sys.argv[2]
    assert mode in ("honest", "lying"), "mode must be honest|lying"
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
                if mode == "honest" and task_id in WORK:
                    fname, content = WORK[task_id]
                    # Workspace is a git worktree; wait for it to appear.
                    for _ in range(100):
                        if os.path.isdir(ws_path):
                            break
                        time.sleep(0.1)
                    with open(os.path.join(ws_path, fname), "w") as f:
                        f.write(content + "\n")
                    print(f"[{mode}] {task_id}: wrote {fname} in {ws_id}", flush=True)
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
