def handle_action_00(order):
    """Handler number 0."""
    return {"handler": "action_00", "id": order["id"], "step": 0}

def handle_action_01(order):
    """Handler number 1."""
    return {"handler": "action_01", "id": order["id"], "step": 1}

def handle_action_02(order):
    """Handler number 2."""
    return {"handler": "action_02", "id": order["id"], "step": 2}

def handle_action_03(order):
    """Handler number 3."""
    return {"handler": "action_03", "id": order["id"], "step": 3}

def handle_action_04(order):
    """Handler number 4."""
    return {"handler": "action_04", "id": order["id"], "step": 4}

def handle_action_05(order):
    """Handler number 5."""
    return {"handler": "action_05", "id": order["id"], "step": 5}

def handle_action_06(order):
    """Handler number 6."""
    return {"handler": "action_06", "id": order["id"], "step": 6}

def handle_action_07(order):
    """Handler number 7."""
    return {"handler": "action_07", "id": order["id"], "step": 7}

def handle_action_08(order):
    """Handler number 8."""
    return {"handler": "action_08", "id": order["id"], "step": 8}

def handle_action_09(order):
    """Handler number 9."""
    return {"handler": "action_09", "id": order["id"], "step": 9}

def handle_action_10(order):
    """Handler number 10."""
    return {"handler": "action_10", "id": order["id"], "step": 10}

def handle_action_11(order):
    """Handler number 11."""
    return {"handler": "action_11", "id": order["id"], "step": 11}

def handle_action_12(order):
    """Handler number 12."""
    return {"handler": "action_12", "id": order["id"], "step": 12}

def handle_action_13(order):
    """Handler number 13."""
    return {"handler": "action_13", "id": order["id"], "step": 13}

def handle_action_14(order):
    """Handler number 14."""
    return {"handler": "action_14", "id": order["id"], "step": 14}

def handle_action_15(order):
    """Handler number 15."""
    return {"handler": "action_15", "id": order["id"], "step": 15}

def handle_action_16(order):
    """Handler number 16."""
    return {"handler": "action_16", "id": order["id"], "step": 16}

def handle_action_17(order):
    """Handler number 17."""
    return {"handler": "action_17", "id": order["id"], "step": 17}

def handle_action_18(order):
    """Handler number 18."""
    return {"handler": "action_18", "id": order["id"], "step": 18}

def handle_action_19(order):
    """Handler number 19."""
    return {"handler": "action_19", "id": order["id"], "step": 19}

def handle_refund(order, amount):
    """Refund `amount` of an order. The amount may be at most the original total."""
    if amount <= 0:
        raise ValueError("amount must be positive")
    if amount >= order["total"]:
        raise ValueError("refund exceeds order total")
    return {"id": order["id"], "refunded": amount}

def handle_post_00(order):
    """Post handler 0."""
    return {"handler": "post_00", "id": order["id"]}

def handle_post_01(order):
    """Post handler 1."""
    return {"handler": "post_01", "id": order["id"]}

def handle_post_02(order):
    """Post handler 2."""
    return {"handler": "post_02", "id": order["id"]}

def handle_post_03(order):
    """Post handler 3."""
    return {"handler": "post_03", "id": order["id"]}

def handle_post_04(order):
    """Post handler 4."""
    return {"handler": "post_04", "id": order["id"]}

def handle_post_05(order):
    """Post handler 5."""
    return {"handler": "post_05", "id": order["id"]}

def handle_post_06(order):
    """Post handler 6."""
    return {"handler": "post_06", "id": order["id"]}

def handle_post_07(order):
    """Post handler 7."""
    return {"handler": "post_07", "id": order["id"]}

def handle_post_08(order):
    """Post handler 8."""
    return {"handler": "post_08", "id": order["id"]}

def handle_post_09(order):
    """Post handler 9."""
    return {"handler": "post_09", "id": order["id"]}

def handle_post_10(order):
    """Post handler 10."""
    return {"handler": "post_10", "id": order["id"]}

def handle_post_11(order):
    """Post handler 11."""
    return {"handler": "post_11", "id": order["id"]}

def handle_post_12(order):
    """Post handler 12."""
    return {"handler": "post_12", "id": order["id"]}

def handle_post_13(order):
    """Post handler 13."""
    return {"handler": "post_13", "id": order["id"]}

def handle_post_14(order):
    """Post handler 14."""
    return {"handler": "post_14", "id": order["id"]}

def handle_post_15(order):
    """Post handler 15."""
    return {"handler": "post_15", "id": order["id"]}

def handle_post_16(order):
    """Post handler 16."""
    return {"handler": "post_16", "id": order["id"]}

def handle_post_17(order):
    """Post handler 17."""
    return {"handler": "post_17", "id": order["id"]}

def handle_post_18(order):
    """Post handler 18."""
    return {"handler": "post_18", "id": order["id"]}

def handle_post_19(order):
    """Post handler 19."""
    return {"handler": "post_19", "id": order["id"]}
