import notify as n

def welcome(user):
    n.send(user, "welcome")
    n.send("admin", f"new user {user}")
