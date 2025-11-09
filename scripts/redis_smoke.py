#!/usr/bin/env python3
"""
Lightweight Redis smoke test for tower-http-cache's Redis example.

The script:
1. (Optionally) starts the Axum Redis example via `cargo run`.
2. Waits for the HTTP endpoint to become available.
3. Issues a sequence of requests to validate cache hits, bypass, and expiry.
4. Shuts down the example process.

Usage:
    python scripts/redis_smoke.py
    python scripts/redis_smoke.py --server-cmd "cargo run --example axum_redis --features redis-backend"
    python scripts/redis_smoke.py --use-existing
"""

from __future__ import annotations

import argparse
import contextlib
import os
import signal
import subprocess
import sys
import time
from dataclasses import dataclass
from urllib.error import URLError
from urllib.parse import urlparse
from urllib.request import Request, urlopen


DEFAULT_SERVER_CMD = [
    "cargo",
    "run",
    "--example",
    "redis_smoke",
    "--features",
    "redis-backend",
]


@dataclass
class Response:
    status: int
    body: str


ALLOWED_HTTP_HOSTS = {"127.0.0.1", "localhost", "::1"}


def validate_http_url(url: str) -> str:
    parsed = urlparse(url)
    if parsed.scheme not in {"http", "https"}:
        raise argparse.ArgumentTypeError("URL must use http:// or https://")

    hostname = parsed.hostname
    if hostname not in ALLOWED_HTTP_HOSTS:
        raise argparse.ArgumentTypeError(
            f"Refusing to use non-local URL '{url}'. "
            "Pass --url with localhost or 127.0.0.1"
        )

    return url


def fetch(url: str, *, headers: dict[str, str] | None = None, timeout: float = 5.0) -> Response:
    req = Request(url, headers=headers or {})
    with urlopen(req, timeout=timeout) as resp:  # nosec B310 (controlled URL)
        return Response(status=resp.getcode(), body=resp.read().decode("utf-8"))


def wait_for_service(url: str, timeout: float = 20.0) -> None:
    start = time.perf_counter()
    while True:
        try:
            fetch(url, timeout=2.0)
            return
        except URLError:
            if time.perf_counter() - start > timeout:
                raise RuntimeError(f"Service at {url} did not become ready within {timeout} seconds")
            time.sleep(0.5)


def run_sequence(url: str) -> None:
    miss = fetch(url)
    if "#1" not in miss.body:
        raise AssertionError(f"expected first response to be backend call #1, got: {miss.body}")

    hit = fetch(url)
    if hit.body != miss.body:
        raise AssertionError("expected cached response to match first response")

    bypass = fetch(url, headers={"Cache-Control": "no-cache"})
    if "#2" not in bypass.body:
        raise AssertionError(f"expected bypass response to be backend call #2, got: {bypass.body}")

    time.sleep(6)  # TTL in example is 5 seconds
    refreshed = fetch(url)
    if "#3" not in refreshed.body:
        raise AssertionError(f"expected refreshed response to be backend call #3, got: {refreshed.body}")


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description="Redis smoke test for tower-http-cache")
    parser.add_argument(
        "--url",
        default="http://127.0.0.1:3000/",
        help="HTTP endpoint exposed by the example service",
    )
    parser.add_argument(
        "--redis-url",
        default="redis://127.0.0.1:6379/",
        help="Redis connection string (forwarded to the example process)",
    )
    parser.add_argument(
        "--server-cmd",
        default=" ".join(DEFAULT_SERVER_CMD),
        help="Command used to launch the Axum Redis example (set to empty to skip)",
    )
    parser.add_argument(
        "--use-existing",
        action="store_true",
        help="Assume the example server is already running; do not spawn a process",
    )
    args = parser.parse_args()
    args.url = validate_http_url(args.url)
    return args


@contextlib.contextmanager
def maybe_launch_server(cmd: list[str], *, env: dict[str, str]) -> subprocess.Popen | None:
    if not cmd:
        yield None
        return

    proc = subprocess.Popen(cmd, env=env)  # noqa: S603
    try:
        yield proc
    finally:
        with contextlib.suppress(ProcessLookupError):
            proc.send_signal(signal.SIGINT)
            try:
                proc.wait(timeout=5)
            except subprocess.TimeoutExpired:
                proc.kill()


def main() -> int:
    args = parse_args()
    cmd: list[str] = [] if args.use_existing else args.server_cmd.split()

    env = os.environ.copy()
    env.setdefault("REDIS_URL", args.redis_url)

    with maybe_launch_server(cmd, env=env) as proc:
        # Allow server to boot before polling.
        if proc is not None:
            time.sleep(1.0)

        wait_for_service(args.url)
        run_sequence(args.url)

    print("✅ Redis smoke test passed")
    return 0


if __name__ == "__main__":
    sys.exit(main())

