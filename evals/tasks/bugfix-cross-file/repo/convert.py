from units import FACTORS

def convert(value, src, dst):
    return value * FACTORS[src] / FACTORS[dst]
