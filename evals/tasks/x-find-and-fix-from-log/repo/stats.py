def percentile(values, p):
    ordered = sorted(values)
    k = round(len(ordered) * p / 100)
    return ordered[k]

def mean(values):
    return sum(values) / len(values)

def summarize(rows):
    values = [r["ms"] for r in rows]
    return {"mean": mean(values), "p50": percentile(values, 50)}
