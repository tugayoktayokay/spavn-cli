import notify

def charge(user, amount):
    notify.send(user, f"charged {amount}")
    return amount
