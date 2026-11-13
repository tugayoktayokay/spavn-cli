def count_pairs(numbers, target):
    """Number of index pairs i < j with numbers[i] + numbers[j] == target."""
    count = 0
    for i in range(len(numbers)):
        for j in range(i + 1, len(numbers)):
            if numbers[i] + numbers[j] == target:
                count += 1
    return count
