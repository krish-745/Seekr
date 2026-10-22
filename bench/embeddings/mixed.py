"""Can the panel embed queries with a smaller copy of gte-modernbert-base than the indexer uses for
documents? Documents in fp32 on the RTX 5050; queries in fp32, int8 and fp16 on the CPU, scored on
SciFact, with each query model's latency and memory.

    uv run --project bench/embeddings python bench/embeddings/mixed.py
"""

import os
import time

import numpy as np
from huggingface_hub import hf_hub_download

import bench

NAME = "gte-modernbert-base"
REPO = bench.CANDIDATES[NAME][0]


def private_mb():
    import ctypes
    from ctypes import wintypes

    class Counters(ctypes.Structure):
        _fields_ = [("cb", wintypes.DWORD), ("PageFaultCount", wintypes.DWORD)] + [
            (n, ctypes.c_size_t) for n in ("PeakWorkingSetSize", "WorkingSetSize", "QuotaPeakPagedPoolUsage",
                                           "QuotaPagedPoolUsage", "QuotaPeakNonPagedPoolUsage",
                                           "QuotaNonPagedPoolUsage", "PagefileUsage", "PeakPagefileUsage",
                                           "PrivateUsage")]
    c = Counters()
    c.cb = ctypes.sizeof(c)
    ctypes.windll.psapi.GetProcessMemoryInfo(ctypes.windll.kernel32.GetCurrentProcess(), ctypes.byref(c), c.cb)
    return c.PrivateUsage / 2**20


def main():
    docs, queries, rel = bench.fetch_scifact()
    doc_ids = list(docs)
    query_ids = list(queries)
    full = bench.Embedder(NAME, [("DmlExecutionProvider", {"device_id": 0})])
    d = full.docs([docs[i] for i in doc_ids])
    del full
    for variant in ["onnx/model.onnx", "onnx/model_int8.onnx", "onnx/model_fp16.onnx"]:
        hf_hub_download(REPO, variant, local_dir=bench.MODELS / NAME)
        bench.CANDIDATES[NAME] = (REPO, variant, "cls", "", "")
        before = private_mb()
        emb = bench.Embedder(NAME, ["CPUExecutionProvider"])
        emb.queries(["warm up"])
        loaded = private_mb() - before
        q = emb.queries([queries[i] for i in query_ids])
        score = bench.ndcg_at_10(q @ d.T, doc_ids, query_ids, rel)
        times = []
        for text in ["quarterly budget review", "how to reset the router", "invoice from march"] * 10:
            start = time.perf_counter()
            emb.queries([text])
            times.append((time.perf_counter() - start) * 1000)
        times.sort()
        print(f"{variant}: nDCG@10 {score:.4f}, query p50 {times[15]:.1f} ms p95 {times[28]:.1f} ms, "
              f"+{loaded:.0f} MB private", flush=True)
        del emb


if __name__ == "__main__":
    os.chdir(os.path.dirname(os.path.abspath(__file__)))
    main()
