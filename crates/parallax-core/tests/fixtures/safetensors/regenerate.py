#!/usr/bin/env python3
"""Builds the safetensors files the tests read (checked in, so tests need no Python).

  tiny-v1.safetensors        a 2-layer llama-like model with tiny tensors (F32, F16, I64)
  tiny-v1-meta.safetensors   the same tensors, other __metadata__ (header length differs)
  tiny-v2.safetensors        one weight changed, one tensor in another dtype, one tensor added
"""
import os

import numpy as np
from safetensors.numpy import save_file

here = os.path.dirname(os.path.abspath(__file__))
rng = np.random.default_rng(42)


def model():
    t = {
        "model.embed_tokens.weight": rng.standard_normal((16, 8)).astype(np.float16),
        "model.norm.weight": np.ones(8, np.float32),
        "lm_head.weight": rng.standard_normal((16, 8)).astype(np.float16),
        "position_ids": np.arange(4, dtype=np.int64),
    }
    for layer in (0, 1, 10):
        p = f"model.layers.{layer}."
        t[p + "self_attn.q_proj.weight"] = rng.standard_normal((8, 8)).astype(np.float16)
        t[p + "self_attn.k_proj.weight"] = rng.standard_normal((4, 8)).astype(np.float16)
        t[p + "self_attn.v_proj.weight"] = rng.standard_normal((4, 8)).astype(np.float16)
        t[p + "mlp.down_proj.weight"] = rng.standard_normal((8, 12)).astype(np.float32)
        t[p + "input_layernorm.weight"] = np.ones(8, np.float32)
    return t


v1 = model()
save_file(v1, os.path.join(here, "tiny-v1.safetensors"), metadata={"format": "pt", "note": "v1"})
save_file(v1, os.path.join(here, "tiny-v1-meta.safetensors"), metadata={"format": "pt", "note": "re-exported", "tool": "x"})
v2 = dict(v1)
w = v2["model.layers.1.self_attn.q_proj.weight"].copy()
w[0, 0] += 1
v2["model.layers.1.self_attn.q_proj.weight"] = w
v2["model.layers.0.mlp.down_proj.weight"] = v2["model.layers.0.mlp.down_proj.weight"].astype(np.float16)
v2["model.layers.0.mlp.up_proj.weight"] = rng.standard_normal((12, 8)).astype(np.float16)
save_file(v2, os.path.join(here, "tiny-v2.safetensors"), metadata={"format": "pt", "note": "v1"})
