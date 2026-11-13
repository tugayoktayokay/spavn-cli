COUPONS = {"TEN": ("pct", 10), "FIVE": ("flat", 5)}

def apply_coupon(amount, code):
    if code is None:
        return amount
    kind, value = COUPONS[code]
    if kind == "pct":
        return round(amount * (1 - value / 10), 2)
    if kind == "flat":
        return max(amount - value, 0)
    raise ValueError(kind)
