# AI answers: model and runtime

Seekr's AI answers read a few thousand tokens of your files and answer with citations. The model
and the runtime that serves it were chosen by benchmark on this laptop (RTX 5050 Laptop GPU with
8 GB, Ryzen 7 250, 24 GB RAM, driver 596.49), with `bench/llm/bench.py`.

- **Task:** 22 questions about Seekr, answered from its README and CHANGELOG at commit `eaa6dc8`.
  Each question gets about 3,000 tokens of numbered passages (those sharing the most words with
  it) and must cite them. Twelve are single facts, eight are exact numbers among many or several
  facts at once, and two have no answer in the passages. No model can know the answers
  beforehand.
- **Right:** the answer contains every expected fact, or for the two unanswerable questions says
  it doesn't know. **Citations:** the answer cites a passage that holds the facts.
- **Cold:** from starting the runtime (or, for Ollama, its unloading the model) to the first
  token of the first answer, model load included, with the model file in the Windows file cache.
  The very first Ollama load after its download took 49 s from disk. **Warm:** the other 21
  questions, each a new prompt, so nothing is cached between them.
- **Runtimes:** llama.cpp `llama-server` build b11819 (CUDA 13.4) with all layers on the GPU,
  flash attention and a q8_0 KV cache, 8k context; Ollama 0.40.6 with its defaults. Thinking is
  off for both, temperature 0.2, at most 400 tokens per answer.
- **Models:** Qwen3.5 9B as Ollama installs it (Q4_K_M with a vision projector), the same GGUF in
  llama-server, and unsloth's Qwen3.5-9B IQ4_XS (revision `3885219`) and Qwen3.5-4B Q4_K_M
  (revision `e87f176`).

| Candidate | Right | Citations | Cold first token | Warm first token (p50 / max) | Speed | Answer (p50) | VRAM peak | GPU power |
|---|---|---|---|---|---|---|---|---|
| **llama-server, Qwen3.5-4B Q4_K_M** | 22/22 | 20/20 | **4.2 s** | **1.14 / 1.33 s** | **74 tok/s** | 1.46 s | 4.0 GB | 76 W |
| llama-server, Qwen3.5-9B IQ4_XS | 22/22 | 20/20 | 5.5 s | 1.64 / 1.76 s | 62 tok/s | 1.98 s | 5.8 GB | 85 W |
| llama-server, Qwen3.5-9B Q4_K_M | 22/22 | 19/20 | 8.4 s | 1.89 / 2.02 s | 49 tok/s | 2.27 s | 6.2 GB | 81 W |
| Ollama, qwen3.5:9b | 22/22 | 20/20 | 9.0 s | 2.07 / 2.18 s | 31 tok/s | 2.76 s | 7.0 GB | 65 W |

Targets: cold ≤ 6 s, warm ≤ 1.5 s, ≥ 40 tok/s, everything in VRAM.

- The warm first token is almost all prompt processing: about 3,000 tokens at roughly 1,900
  tokens a second for the 9B models. Larger batches (`-ub 1024`, `-ub 2048`) gained 0.06 s.
- The same Q4_K_M file is 1.6 times faster in llama-server than in Ollama, and takes 0.8 GB less
  memory (Ollama also loads the vision projector).
- Every runtime released the GPU when stopped; it went back to its low-power state.

## Decision

**Qwen3.5-4B Q4_K_M in llama-server** is the default: the only candidate inside every target,
as accurate as the 9B models on this task, and leaving 4 GB of VRAM free. Qwen3.5-9B IQ4_XS is the
step up for harder questions (0.14 s over the warm target). Ollama stays usable for people who
already run it, but isn't the default.

Cloud APIs answer only general `??` questions, which never carry file context, so they aren't
compared on this task.

## Phase 5 gate (Seekr's own code)

`cargo test -p seekr phase5_gate -- --ignored --nocapture` runs the default model through the
same functions `?` uses, with about 3,000 tokens of passages per question (the first 12,000
characters of a file), on the RTX 5050 Laptop GPU (8 GB), on top of 6560794:

| Measure | Result | Target |
|---|---|---|
| Cold first word (server start included) | 3.8 s | ≤ 6 s |
| Warm first word, new passages | 0.54 s, 0.76 s | ≤ 1.5 s |
| Speed | 79 tokens/s | ≥ 40 tokens/s |
| GPU memory while loaded | 4.0 GB, all on the GPU | fits in VRAM |
| GPU state after the model stops | P8 after 4.3 s | low power |
