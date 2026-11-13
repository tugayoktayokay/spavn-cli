def parse_kv(line):
    """Parse 'a=1;b=2' into {'a': '1', 'b': '2'}."""
    out = {}
    for part in line.split(";"):
        key, value = part.split("=")
        out[key] = value
    return out
