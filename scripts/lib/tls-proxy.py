#!/usr/bin/env python3
"""Minimal TLS-terminating TCP proxy for local testing.

The `gh` CLI only talks HTTPS to GitHub Enterprise hosts
(`https://HOST/api/v3/`, `https://HOST/api/graphql`), so the compatibility
harness puts this in front of a plain-HTTP `bgh` server:

    tls-proxy.py --listen 127.0.0.1:8443 --upstream 127.0.0.1:3000 \
        --cert cert.pem --key key.pem [--ready-file PATH]

Bytes are relayed unchanged in both directions (HTTP/1.1 keep-alive, chunked
git packs and WebSocket upgrades all just work). Standard library only.
"""

import argparse
import asyncio
import signal
import ssl
import sys


def host_port(value: str) -> tuple[str, int]:
    host, _, port = value.rpartition(":")
    return host.strip("[]") or "127.0.0.1", int(port)


async def pump(reader: asyncio.StreamReader, writer: asyncio.StreamWriter) -> None:
    try:
        while True:
            data = await reader.read(65536)
            if not data:
                break
            writer.write(data)
            await writer.drain()
    except (ConnectionError, ssl.SSLError, asyncio.IncompleteReadError):
        pass
    finally:
        try:
            if writer.can_write_eof():
                writer.write_eof()
            else:
                writer.close()
        except (OSError, RuntimeError, ssl.SSLError):
            pass


async def handle(client_r, client_w, upstream: tuple[str, int]) -> None:
    try:
        up_r, up_w = await asyncio.open_connection(*upstream)
    except OSError as err:
        print(f"tls-proxy: upstream {upstream}: {err}", file=sys.stderr)
        client_w.close()
        return
    try:
        await asyncio.gather(pump(client_r, up_w), pump(up_r, client_w))
    finally:
        for w in (up_w, client_w):
            try:
                w.close()
            except (OSError, RuntimeError, ssl.SSLError):
                pass


async def main() -> None:
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("--listen", required=True, help="HOST:PORT to accept TLS on")
    ap.add_argument("--upstream", required=True, help="HOST:PORT of the plain-HTTP server")
    ap.add_argument("--cert", required=True)
    ap.add_argument("--key", required=True)
    ap.add_argument("--ready-file", help="written once the listener is up")
    args = ap.parse_args()

    ctx = ssl.create_default_context(ssl.Purpose.CLIENT_AUTH)
    ctx.load_cert_chain(args.cert, args.key)
    ctx.set_alpn_protocols(["http/1.1"])
    upstream = host_port(args.upstream)
    host, port = host_port(args.listen)

    server = await asyncio.start_server(
        lambda r, w: handle(r, w, upstream), host, port, ssl=ctx
    )
    if args.ready_file:
        with open(args.ready_file, "w") as f:
            f.write("ready\n")
    stop = asyncio.Event()
    loop = asyncio.get_running_loop()
    for sig in (signal.SIGINT, signal.SIGTERM):
        loop.add_signal_handler(sig, stop.set)
    async with server:
        await stop.wait()


if __name__ == "__main__":
    asyncio.run(main())
