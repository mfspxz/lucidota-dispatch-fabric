#!/bin/sh
# dogfood verifier: file-exists capability.
# Invoked with the task workspace as cwd; argv[1] is the relative path
# (the part after ':' in the acceptance predicate, e.g. "file-exists:one.txt").
# Exit 0 iff the file exists and is a regular file. No output on success.
set -u
if [ $# -ne 1 ]; then
  echo "usage: check-file-exists.sh <relative-path>" >&2
  exit 2
fi
[ -f "$1" ] && exit 0
echo "missing: $1" >&2
exit 1
