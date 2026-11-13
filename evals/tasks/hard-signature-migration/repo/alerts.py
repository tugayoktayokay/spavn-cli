from notify import send

def alert_all(users, text):
    return [send(u, text) for u in users]
