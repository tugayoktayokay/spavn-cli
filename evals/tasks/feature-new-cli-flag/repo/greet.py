import argparse

def build_greeting(name):
    return f"Hello, {name}!"

def main(argv=None):
    parser = argparse.ArgumentParser()
    parser.add_argument("name")
    args = parser.parse_args(argv)
    print(build_greeting(args.name))

if __name__ == "__main__":
    main()
