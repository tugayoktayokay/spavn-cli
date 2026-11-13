def moving_sum(values, size):
    """Sums of every run of `size` consecutive values."""
    return [sum(values[i:i + size]) for i in range(len(values) - size)]
