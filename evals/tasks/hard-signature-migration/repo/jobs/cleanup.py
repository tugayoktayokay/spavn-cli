from notify import send as dispatch

def run(owner):
    return dispatch(
        owner,
        "cleanup done",
    )
