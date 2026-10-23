# Embedding model and device

Meaning search turns each passage and each query into a vector with an embedding model. The
model and the device it runs on were chosen by benchmark on one laptop (Ryzen 7 250 with Radeon
780M, RTX 5050 Laptop GPU, 24 GB RAM), with `bench/embeddings/bench.py` and `mixed.py`.

- **Quality:** nDCG@10 on the [BEIR SciFact](https://huggingface.co/datasets/mteb/scifact) test
  set (300 queries over 5,183 abstracts), fp32 models.
- **Indexing speed:** passages per second, batches of 16, up to 512 tokens, ONNX Runtime 1.24
  (CPU and DirectML providers). DirectML adapter 0 is the RTX 5050, adapter 1 the Radeon 780M
  (identified by NVIDIA utilisation during each run).
- **Query time:** one short query on the CPU, which every keystroke pays.

| Model | nDCG@10 | Query (CPU, p50) | CPU | RTX 5050 | Radeon 780M |
|---|---|---|---|---|---|
| bge-small-en-v1.5 | 0.713 | 6.7 ms | 7.4/s | 72.0/s | 10.5/s |
| nomic-embed-text-v1.5 | 0.703 | 24.3 ms | 2.1/s | 23.4/s | 4.1/s |
| **gte-modernbert-base** | **0.765** | 24.6 ms | 2.0/s | 19.7/s | 2.8/s |

Queries can use a smaller copy of gte-modernbert-base than the documents (fp32 documents on the
RTX 5050, queries on the CPU):

| Query model | nDCG@10 | Query p50 / p95 | File |
|---|---|---|---|
| fp32 | 0.765 | 23.4 / 24.5 ms | 596 MB |
| **int8** | **0.746** | 10.7 / 11.5 ms | 150 MB |
| fp16 | does not load on the CPU provider | | 298 MB |

## Decision

- **Model:** gte-modernbert-base (768 dimensions, CLS pooling, no prefixes). It finds the right
  document best by a clear margin; bge-small is faster but five points worse, and nomic is worse
  than bge-small on both counts.
- **Documents:** the fp32 model, in bulk on the RTX 5050 through DirectML (about 10× the CPU and
  7× the 780M). A run with only a few passages uses the CPU, so the GPU is woken only for bulk
  work, and its session ends with the indexer process. The RTX 5050 was back in P8 (about 5 W)
  after every run.
- **Queries:** the int8 model on the CPU in the panel: 0.746 still beats every other model, at
  under half the time and a quarter of the memory of fp32.
