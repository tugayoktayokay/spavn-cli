class InsufficientFunds(Exception):
    pass


class Bank:
    def __init__(self):
        self.balances = {}
        self.ledger = []

    def open(self, name, amount=0):
        self.balances[name] = amount

    def transfer(self, src, dst, amount):
        if amount <= 0:
            raise ValueError("amount must be positive")
        self.balances[src] -= amount
        if self.balances[src] < 0:
            raise InsufficientFunds(src)
        self.balances[dst] += amount
        self.ledger.append((src, dst, amount))
