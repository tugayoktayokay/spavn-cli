from billing import calc_total

def summary(prices):
    return f"Total: {calc_total(prices)}"
