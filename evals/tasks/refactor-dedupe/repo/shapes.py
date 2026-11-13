def area_square(side):
    if side < 0:
        raise ValueError("negative")
    return side * side

def area_rect(w, h):
    if w < 0 or h < 0:
        raise ValueError("negative")
    return w * h
