from pricing import apply_coupon

def total(items, coupon=None):
    subtotal = sum(i["price"] * i["qty"] for i in items)
    return apply_coupon(subtotal, coupon)
