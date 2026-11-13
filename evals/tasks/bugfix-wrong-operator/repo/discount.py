def final_price(price, percent):
    """Price after a percentage discount, rounded to cents."""
    return round(price - price * percent, 2)
