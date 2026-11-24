#!/usr/bin/env python3
"""Concurrent search load: many clients, each tied to one tenant, asking at once.

usage: load.py HOST PORT TARGETS_FILE CONCURRENCY SECONDS
TARGETS_FILE has one "api_key index_uid" per line; client i uses line i % n.
Prints one line: requests/s, latency percentiles in ms, and errors.
"""
import http.client
import json
import multiprocessing as mp
import random
import sys
import threading
import time

QUERIES = [
    {"q": "motor"},
    {"q": "motor búsqueda"},
    {"q": "servidr rapdo"},
    {"q": "compre"},
    {"q": "\"precio calidad\""},
    {"q": "portátil solar eléctrico manual", "matchingStrategy": "all"},
    {"q": "casa 12345"},
    {"q": "", "filter": "category = Libros AND price < 100", "sort": ["price:asc"]},
    {"q": "garantía", "facets": ["category"], "filter": "stock > 400"},
    {"q": "descuento envío", "sort": ["created:desc"]},
]
BODIES = [json.dumps(dict(q, limit=10, attributesToRetrieve=["id"])).encode() for q in QUERIES]


def client(host, port, key, index, seed, stop_at, out):
    rnd = random.Random(seed)
    headers = {"Authorization": "Bearer " + key, "Content-Type": "application/json"}
    path = "/indexes/%s/search" % index
    conn = http.client.HTTPConnection(host, port, timeout=30)
    lat, errors = [], 0
    while time.time() < stop_at:
        body = BODIES[rnd.randrange(len(BODIES))]
        t0 = time.perf_counter()
        try:
            conn.request("POST", path, body, headers)
            resp = conn.getresponse()
            resp.read()
            if resp.status != 200:
                errors += 1
        except Exception:
            errors += 1
            conn.close()
            conn = http.client.HTTPConnection(host, port, timeout=30)
            continue
        lat.append(time.perf_counter() - t0)
    out.append((lat, errors))


def process(host, port, jobs, stop_at, queue):
    out, threads = [], []
    for key, index, seed in jobs:
        t = threading.Thread(target=client, args=(host, port, key, index, seed, stop_at, out))
        t.start()
        threads.append(t)
    for t in threads:
        t.join()
    queue.put(([x for lat, _ in out for x in lat], sum(e for _, e in out)))


def main():
    host, port, targets_file, conc, seconds = sys.argv[1], int(sys.argv[2]), sys.argv[3], int(sys.argv[4]), float(sys.argv[5])
    targets = [line.split() for line in open(targets_file) if line.strip()]
    jobs = [(targets[i % len(targets)][0], targets[i % len(targets)][1], i) for i in range(conc)]
    nproc = min(conc, 16)
    stop_at = time.time() + seconds
    queue = mp.Queue()
    procs = [mp.Process(target=process, args=(host, port, jobs[p::nproc], stop_at, queue)) for p in range(nproc)]
    for p in procs:
        p.start()
    lat, errors = [], 0
    for _ in procs:
        l, e = queue.get()
        lat.extend(l)
        errors += e
    for p in procs:
        p.join()
    lat.sort()
    pct = lambda q: lat[min(len(lat) - 1, int(len(lat) * q))] * 1000 if lat else 0.0
    print("conc=%-4d rps=%-8.0f p50=%-7.2f p95=%-7.2f p99=%-7.2f max=%-8.2f errors=%d" % (conc, len(lat) / seconds, pct(0.50), pct(0.95), pct(0.99), pct(1.0), errors))


if __name__ == "__main__":
    main()
