_DEFAULTS = {"retries": 3, "tags": []}
current = _DEFAULTS

def get(key):
    return current[key]

def override(**values):
    current.update(values)
    return current

def reset():
    global current
    current = _DEFAULTS
