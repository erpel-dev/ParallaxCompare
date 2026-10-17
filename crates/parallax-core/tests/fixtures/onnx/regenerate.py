#!/usr/bin/env python3
"""Builds the ONNX models the tests read (checked in, so tests need no Python).

  mlp-v1.onnx   a two-layer MLP: Gemm (with attributes), Relu, Gemm, Softmax; a dynamic batch
                dimension, float and int64 initializers, metadata props, a second opset domain
  mlp-v2.onnx   v1 with one weight changed and an extra node (Dropout before the last Gemm)
"""
import os

import numpy as np
import onnx
from onnx import TensorProto, helper, numpy_helper

here = os.path.dirname(os.path.abspath(__file__))
rng = np.random.default_rng(7)
W1 = rng.standard_normal((4, 8)).astype(np.float32)
B1 = np.zeros(8, np.float32)
W2 = rng.standard_normal((8, 3)).astype(np.float32)
B2 = np.full(3, 0.5, np.float32)


def build(version):
    w2 = W2.copy()
    if version == 2:
        w2[1, 2] = 42.0
    inits = [
        numpy_helper.from_array(W1, "fc1.weight"),
        numpy_helper.from_array(B1, "fc1.bias"),
        numpy_helper.from_array(w2, "fc2.weight"),
        numpy_helper.from_array(B2, "fc2.bias"),
        numpy_helper.from_array(np.array([-1, 3], np.int64), "shape"),
    ]
    nodes = [
        helper.make_node("Gemm", ["x", "fc1.weight", "fc1.bias"], ["h1"], name="fc1", alpha=1.0, beta=1.0, transB=0),
        helper.make_node("Relu", ["h1"], ["a1"], name="relu"),
    ]
    last = "a1"
    if version == 2:
        nodes.append(helper.make_node("Dropout", ["a1"], ["d1"], name="dropout"))
        last = "d1"
    nodes += [
        helper.make_node("Gemm", [last, "fc2.weight", "fc2.bias"], ["logits"], name="fc2"),
        helper.make_node("Reshape", ["logits", "shape"], ["flat"], name="reshape"),
        helper.make_node("Softmax", ["flat"], ["probs"], name="softmax", axis=-1),
    ]
    graph = helper.make_graph(
        nodes,
        "mlp",
        [helper.make_tensor_value_info("x", TensorProto.FLOAT, ["batch", 4])],
        [helper.make_tensor_value_info("probs", TensorProto.FLOAT, ["batch", 3])],
        inits,
    )
    model = helper.make_model(
        graph,
        producer_name="regenerate.py",
        producer_version="1.0",
        opset_imports=[helper.make_opsetid("", 17), helper.make_opsetid("ai.onnx.ml", 3)],
        doc_string="A tiny MLP",
    )
    model.ir_version = 8
    helper.set_model_props(model, {"author": "tests", "version": str(version)})
    onnx.checker.check_model(model)
    onnx.save(model, os.path.join(here, f"mlp-v{version}.onnx"))


build(1)
build(2)
