"""Parse Prometheus text exposition from a file with the reference parser.

Prints one line per family and per sample, in a canonical order, so the
caller can compare against an expected summary. Exits nonzero (with the
parser's message) if the text does not parse.
"""
import math
import sys

from prometheus_client.parser import text_string_to_metric_families


def show(value):
    # The parser answers an int for integer text and a float otherwise; one
    # spelling for both keeps the comparison about the value.
    value = float(value)
    if math.isnan(value):
        return "NaN"
    if math.isinf(value):
        return "+Inf" if value > 0 else "-Inf"
    return repr(value)


text = open(sys.argv[1], encoding="utf-8").read()
for family in text_string_to_metric_families(text):
    print(f"family {family.name} {family.type} {family.documentation!r}")
    for sample in family.samples:
        labels = ",".join(f"{k}={v!r}" for k, v in sorted(sample.labels.items()))
        print(f"  {sample.name} {{{labels}}} {show(sample.value)}")
