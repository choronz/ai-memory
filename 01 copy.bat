cargo build --release -p ai-memory-cli
move /y C:\Apps\ai-memory\ai-memory.exe C:\Apps\ai-memory\ai-memory.exe_1
copy /y target\release\ai-memory.exe C:\Apps\ai-memory
copy /y target\release\ai-memory.exe C:\Users\Admin\.cargo\bin
pause
