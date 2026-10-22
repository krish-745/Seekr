"""Picks Seekr's embedding model and indexing device.

Quality: nDCG@10 on the BEIR SciFact test queries (300 queries, 5,183 abstracts), every model in
fp32 on the CPU. Speed: passages per second for bulk indexing on the CPU and on each GPU through
DirectML, and the CPU time to embed one short query, which is what every keystroke pays.

    uv run --project bench/embeddings python bench/embeddings/bench.py

Models and data are cached in %LOCALAPPDATA%\\Seekr\\models and %LOCALAPPDATA%\\Seekr\\bench.
"""

import json
import math
import os
import subprocess
import sys
import threading
import time
from pathlib import Path

import numpy as np
import onnxruntime as ort
from huggingface_hub import hf_hub_download
from tokenizers import Tokenizer

LOCAL = Path(os.environ["LOCALAPPDATA"]) / "Seekr"
MODELS = LOCAL / "models"
DATA = LOCAL / "bench"
MAX_TOKENS = 512
BATCH = 16
SPEED_PASSAGES = 512

# repo, onnx file, pooling, query prefix, document prefix
CANDIDATES = {
    "bge-small-en-v1.5": ("BAAI/bge-small-en-v1.5", "onnx/model.onnx", "cls",
                          "Represent this sentence for searching relevant passages: ", ""),
    "nomic-embed-text-v1.5": ("nomic-ai/nomic-embed-text-v1.5", "onnx/model.onnx", "mean",
                              "search_query: ", "search_document: "),
    "gte-modernbert-base": ("Alibaba-NLP/gte-modernbert-base", "onnx/model.onnx", "cls", "", ""),
}


def fetch_model(name):
    repo, onnx_file, *_ = CANDIDATES[name]
    target = MODELS / name
    model = Path(hf_hub_download(repo, onnx_file, local_dir=target))
    tokenizer = Path(hf_hub_download(repo, "tokenizer.json", local_dir=target))
    # Some exports keep weights in a side file next to the graph.
    try:
        hf_hub_download(repo, onnx_file + "_data", local_dir=target)
    except Exception:
        pass
    return model, tokenizer


def fetch_scifact():
    DATA.mkdir(parents=True, exist_ok=True)
    get = lambda f: hf_hub_download("mteb/scifact", f, repo_type="dataset", local_dir=DATA)
    docs = {}
    with open(get("corpus.jsonl"), encoding="utf-8") as f:
        for line in f:
            d = json.loads(line)
            docs[d["_id"]] = (d.get("title", "") + " " + d["text"]).strip()
    qs = {}
    with open(get("queries.jsonl"), encoding="utf-8") as f:
        for line in f:
            q = json.loads(line)
            qs[q["_id"]] = q["text"]
    rel = {}
    with open(get("qrels/test.tsv"), encoding="utf-8") as f:
        next(f)
        for line in f:
            qid, did, score = line.strip().split("\t")
            if int(score) > 0:
                rel.setdefault(qid, {})[did] = int(score)
    return docs, {q: qs[q] for q in rel}, rel


class Embedder:
    def __init__(self, name, providers):
        model, tokenizer = fetch_model(name)
        _, _, self.pooling, self.query_prefix, self.doc_prefix = CANDIDATES[name]
        self.tokenizer = Tokenizer.from_file(str(tokenizer))
        self.tokenizer.enable_truncation(MAX_TOKENS)
        self.tokenizer.enable_padding()
        options = ort.SessionOptions()
        options.graph_optimization_level = ort.GraphOptimizationLevel.ORT_ENABLE_ALL
        self.session = ort.InferenceSession(str(model), options, providers=providers)
        self.inputs = {i.name for i in self.session.get_inputs()}

    def embed(self, texts):
        enc = self.tokenizer.encode_batch(texts)
        ids = np.array([e.ids for e in enc], dtype=np.int64)
        mask = np.array([e.attention_mask for e in enc], dtype=np.int64)
        feed = {"input_ids": ids, "attention_mask": mask}
        if "token_type_ids" in self.inputs:
            feed["token_type_ids"] = np.zeros_like(ids)
        hidden = self.session.run(None, feed)[0]
        if self.pooling == "cls":
            vec = hidden[:, 0]
        else:
            m = mask[..., None].astype(np.float32)
            vec = (hidden * m).sum(1) / np.maximum(m.sum(1), 1e-9)
        return vec / np.linalg.norm(vec, axis=1, keepdims=True)

    def docs(self, texts):
        return np.concatenate([self.embed([self.doc_prefix + t for t in texts[i:i + BATCH]])
                               for i in range(0, len(texts), BATCH)])

    def queries(self, texts):
        return np.concatenate([self.embed([self.query_prefix + t for t in texts[i:i + BATCH]])
                               for i in range(0, len(texts), BATCH)])


def ndcg_at_10(scores, doc_ids, query_ids, rel):
    total = 0.0
    for row, qid in zip(scores, query_ids):
        top = np.argsort(-row)[:10]
        dcg = sum((2 ** rel[qid].get(doc_ids[i], 0) - 1) / math.log2(r + 2) for r, i in enumerate(top))
        ideal = sorted(rel[qid].values(), reverse=True)[:10]
        idcg = sum((2 ** g - 1) / math.log2(r + 2) for r, g in enumerate(ideal))
        total += dcg / idcg if idcg else 0.0
    return total / len(query_ids)


class GpuWatch:
    """Samples NVIDIA utilisation during a run, to tell which DirectML adapter is the RTX."""

    def __init__(self):
        self.peak = 0
        self._stop = False
        self._thread = threading.Thread(target=self._run, daemon=True)

    def _run(self):
        while not self._stop:
            try:
                out = subprocess.run(["nvidia-smi", "--query-gpu=utilization.gpu", "--format=csv,noheader,nounits"],
                                     capture_output=True, text=True, timeout=5).stdout
                self.peak = max(self.peak, int(out.strip().splitlines()[0]))
            except Exception:
                pass
            time.sleep(0.2)

    def __enter__(self):
        self._thread.start()
        return self

    def __exit__(self, *exc):
        self._stop = True
        self._thread.join()


def speed(name, providers, passages):
    emb = Embedder(name, providers)
    emb.docs(passages[:BATCH])  # warm up
    with GpuWatch() as watch:
        start = time.perf_counter()
        emb.docs(passages)
        elapsed = time.perf_counter() - start
    return len(passages) / elapsed, watch.peak


def query_latency(name):
    emb = Embedder(name, ["CPUExecutionProvider"])
    emb.queries(["warm up"])
    times = []
    for q in ["quarterly budget review", "how to reset the router", "notes about the lighthouse trip",
              "python script that renames photos", "invoice from march"] * 6:
        start = time.perf_counter()
        emb.queries([q])
        times.append((time.perf_counter() - start) * 1000)
    times.sort()
    return times[len(times) // 2], times[int(len(times) * 0.95) - 1]


def main():
    docs, queries, rel = fetch_scifact()
    doc_ids = list(docs)
    doc_texts = [docs[d] for d in doc_ids]
    query_ids = list(queries)
    passages = doc_texts[:SPEED_PASSAGES]
    only = sys.argv[1:] or list(CANDIDATES)
    devices = {"cpu": ["CPUExecutionProvider"],
               "dml0": [("DmlExecutionProvider", {"device_id": 0})],
               "dml1": [("DmlExecutionProvider", {"device_id": 1})]}
    results = {}
    for name in only:
        print(f"== {name}", flush=True)
        q50, q95 = query_latency(name)
        print(f"  query on CPU: p50 {q50:.1f} ms, p95 {q95:.1f} ms", flush=True)
        rates = {}
        for device, providers in devices.items():
            try:
                rate, peak = speed(name, providers, passages)
                rates[device] = (rate, peak)
                print(f"  {device}: {rate:.1f} passages/s (NVIDIA peak {peak}%)", flush=True)
            except Exception as e:
                print(f"  {device}: failed: {e}", flush=True)
        # Quality on the fastest device; embeddings don't depend on it beyond rounding.
        best = max(rates, key=lambda d: rates[d][0])
        emb = Embedder(name, devices[best])
        start = time.perf_counter()
        d = emb.docs(doc_texts)
        q = emb.queries([queries[i] for i in query_ids])
        score = ndcg_at_10(q @ d.T, doc_ids, query_ids, rel)
        print(f"  SciFact nDCG@10 {score:.4f} (embedded on {best} in {time.perf_counter() - start:.0f} s)",
              flush=True)
        results[name] = {"ndcg10": score, "query_ms_p50": q50, "query_ms_p95": q95,
                         "passages_per_s": {k: v[0] for k, v in rates.items()},
                         "nvidia_peak": {k: v[1] for k, v in rates.items()}}
    out = Path(__file__).with_name("results.json")
    out.write_text(json.dumps(results, indent=2))
    print(f"wrote {out}")


if __name__ == "__main__":
    main()
