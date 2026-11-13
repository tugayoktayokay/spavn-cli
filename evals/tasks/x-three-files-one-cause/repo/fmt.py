def pad(text, width):
    data = text.encode("ascii")
    return data.ljust(width).decode("ascii")
