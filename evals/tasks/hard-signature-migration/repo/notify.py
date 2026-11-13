SENT = []

def send(to, text):
    SENT.append((to, text))
    return len(SENT)
