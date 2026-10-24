#!/usr/bin/env python3
"""Bridge a PTY to mdc-sim-peer over TCP for serial integration tests."""
import os
import pty
import select
import socket
import sys

host, port = sys.argv[1].split(":")
master, slave = pty.openpty()
print(os.ttyname(slave), flush=True)
sock = socket.create_connection((host, int(port)))
sock.setblocking(False)
try:
    while True:
        readable, _, _ = select.select([master, sock], [], [], 1.0)
        if master in readable:
            try:
                data = os.read(master, 65536)
            except OSError:
                break
            if not data:
                break
            sock.sendall(data)
        if sock in readable:
            try:
                data = sock.recv(65536)
            except BlockingIOError:
                continue
            if not data:
                break
            os.write(master, data)
finally:
    sock.close()
    os.close(master)
    os.close(slave)
