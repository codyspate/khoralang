"""Requests a second against a server that is already running.

One connection per worker process, held open, request and response in
lockstep. Processes rather than threads because one Python process tops out
well below what the servers here can answer, and one process holding
forty-eight connections measures the client.

    python bench/load.py 18952 "service"

The number this prints is only comparable to another number from the same
machine, the same worker count and the same duration. That is the whole reason
`bench/README.md` records all three beside every figure it quotes.
"""
import multiprocessing as mp
import socket
import sys
import time

REQUEST = b"GET /health HTTP/1.1\r\nHost: x\r\n\r\n"


def worker(port, seconds, out):
    answered = 0
    try:
        connection = socket.create_connection(("127.0.0.1", port), timeout=10)
        connection.setsockopt(socket.IPPROTO_TCP, socket.TCP_NODELAY, 1)
        connection.settimeout(10)
        deadline = time.time() + seconds
        while time.time() < deadline:
            connection.sendall(REQUEST)
            received = b""
            # Read to the end of the headers. Every server here answers in one
            # write, so this loop turns once; it exists so that a server which
            # does not is measured honestly rather than counted early.
            while b"\r\n\r\n" not in received:
                chunk = connection.recv(65536)
                if not chunk:
                    raise ConnectionError("the server closed the connection")
                received += chunk
            answered += 1
    except Exception:
        # A worker that dies contributes what it managed. The count is the
        # measurement; a traceback per process is not.
        pass
    out.put(answered)


if __name__ == "__main__":
    port = int(sys.argv[1])
    label = sys.argv[2] if len(sys.argv) > 2 else "server"
    workers = int(sys.argv[3]) if len(sys.argv) > 3 else 48
    seconds = int(sys.argv[4]) if len(sys.argv) > 4 else 5

    out = mp.Queue()
    running = [mp.Process(target=worker, args=(port, seconds, out)) for _ in range(workers)]
    for process in running:
        process.start()
    counts = [out.get() for _ in running]
    for process in running:
        process.join()

    total = sum(counts)
    print(f"  {label:28s} {total / seconds:8.0f} req/s  ({total} in {seconds}s, {workers} conns)")
