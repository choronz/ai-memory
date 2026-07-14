cargo build --release -p ai-memory-cli
move /y C:\Apps\ai-memory\ai-memory.exe C:\Apps\ai-memory\ai-memory.exe_1
copy /y target\release\ai-memory.exe C:\Apps\ai-memory
pause
