import sys
print("python:", sys.executable)
print("--- start import torch ---")
import torch
print("torch version:", torch.__version__)
print("cuda:", torch.version.cuda)
print("--- end ---")
