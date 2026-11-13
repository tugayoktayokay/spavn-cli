import json

DATA = '[{"name": "Zoë Ångström", "paid": 12.5}, {"name": "Bob", "paid": 3}]'

def load():
    return json.loads(DATA)
