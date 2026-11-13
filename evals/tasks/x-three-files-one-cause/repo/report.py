from fmt import pad

def render(rows):
    lines = [pad(r["name"], 14) + f"{r['paid']:>8.2f}" for r in rows]
    return "\n".join(lines)
