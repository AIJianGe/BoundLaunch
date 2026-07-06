import sys, os
print("=== sys.path ===")
for p in sys.path:
    print(" ", p)
print("=== PYTHONPATH env ===")
print("PYTHONPATH:", os.environ.get("PYTHONPATH", "(not set)"))
print("=== sys.executable ===")
print(sys.executable)
print("=== try import numpy first ===")
try:
    import numpy
    print("numpy loaded from:", numpy.__file__)
    print("numpy version:", numpy.__version__)
except Exception as e:
    print("FAIL import numpy:", type(e).__name__, e)
