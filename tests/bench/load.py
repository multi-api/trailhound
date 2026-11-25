#!/usr/bin/env python3
"""Concurrent search load: many clients, each tied to one tenant, asking at once.

usage: load.py HOST PORT TARGETS_FILE CONCURRENCY SECONDS [QUERIES_FILE]
TARGETS_FILE has one "api_key index_uid" per line; client i uses line i % n.
QUERIES_FILE is a JSON list of search requests (wiki.py writes one); without
it, the queries below, which fit the documents of gen.js.
Prints one line: requests/s, latency percentiles in ms, and errors. When the
queries carry a "kind", one more line per kind.
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
if len(sys.argv) > 6:
    QUERIES = json.load(open(sys.argv[6], encoding="utf-8"))
KINDS = sorted({q.get("kind", "") for q in QUERIES})
KIND = [KINDS.index(q.pop("kind", "")) for q in QUERIES]
BODIES = [json.dumps(dict(q, limit=10, attributesToRetrieve=["id"])).encode() for q in QUERIES]


def client(host, port, key, index, seed, stop_at, out):
    rnd = random.Random(seed)
    headers = {"Authorization": "Bearer " + key, "Content-Type": "application/json"}
    path = "/indexes/%s/search" % index
    conn = http.client.HTTPConnection(host, port, timeout=30)
    lat, errors = [], 0
    while time.time() < stop_at:
        n = rnd.randrange(len(BODIES))
        body = BODIES[n]
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
        lat.append((time.perf_counter() - t0, KIND[n]))
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
    by_kind = [sorted(t for t, k in lat if k == i) for i in range(len(KINDS))]
    lat = sorted(t for t, _ in lat)
    pct = lambda q, l=lat: l[min(len(l) - 1, int(len(l) * q))] * 1000 if l else 0.0
    print("conc=%-4d rps=%-8.0f p50=%-7.2f p95=%-7.2f p99=%-7.2f max=%-8.2f errors=%d" % (conc, len(lat) / seconds, pct(0.50), pct(0.95), pct(0.99), pct(1.0), errors))
    if len(KINDS) > 1:
        for name, l in zip(KINDS, by_kind):
            print("      %-7s n=%-7d p50=%-7.2f p99=%-7.2f" % (name, len(l), pct(0.50, l), pct(0.99, l)))


if __name__ == "__main__":
    main()
