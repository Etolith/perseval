#!/usr/bin/env python3
"""Invoke one Perseval MCP tool over the documented stdio lifecycle."""

from __future__ import annotations

import argparse
import json
import os
from pathlib import Path
import subprocess
import sys


def send(process: subprocess.Popen[str], message: dict) -> None:
    assert process.stdin is not None
    process.stdin.write(json.dumps(message, separators=(",", ":")) + "\n")
    process.stdin.flush()


def receive(process: subprocess.Popen[str], request_id: int) -> dict:
    assert process.stdout is not None
    while line := process.stdout.readline():
        message = json.loads(line)
        if message.get("id") == request_id:
            return message
    raise RuntimeError(f"MCP server closed before response {request_id}")


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--workspace", type=Path, required=True)
    parser.add_argument(
        "--binary", type=Path, default=Path("target/debug/perseval-mcp")
    )
    parser.add_argument("tool", nargs="?", default="tools/list")
    parser.add_argument("arguments", nargs="?", default="{}")
    args = parser.parse_args()
    arguments = json.loads(args.arguments)
    if not isinstance(arguments, dict):
        raise SystemExit("arguments must decode to a JSON object")

    env = os.environ.copy()
    env.update(
        PERSEVAL_WORKSPACE_DIR=str(args.workspace.resolve()),
        PERSEVAL_MCP_READ_ENABLED="true",
    )
    process = subprocess.Popen(
        [str(args.binary.resolve())],
        stdin=subprocess.PIPE,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
        env=env,
    )
    try:
        send(
            process,
            {
                "jsonrpc": "2.0",
                "id": 1,
                "method": "initialize",
                "params": {
                    "protocolVersion": "2025-11-25",
                    "capabilities": {},
                    "clientInfo": {"name": "perseval-smoke", "version": "1"},
                },
            },
        )
        initialized = receive(process, 1)
        if "error" in initialized:
            print(json.dumps(initialized, indent=2))
            raise SystemExit(2)
        send(process, {"jsonrpc": "2.0", "method": "notifications/initialized"})
        if args.tool == "tools/list":
            method, params = "tools/list", {}
        else:
            method = "tools/call"
            params = {"name": args.tool, "arguments": arguments}
        send(process, {"jsonrpc": "2.0", "id": 2, "method": method, "params": params})
        print(json.dumps(receive(process, 2), indent=2))
    finally:
        if process.stdin:
            process.stdin.close()
        try:
            process.wait(timeout=5)
        except subprocess.TimeoutExpired:
            process.terminate()
        if process.stderr:
            diagnostics = process.stderr.read()
            if diagnostics:
                print(diagnostics, file=sys.stderr, end="")


if __name__ == "__main__":
    main()
