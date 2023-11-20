Allow specifying the default `Format` for a target
    
Refactor the target definition to allow specifying the default Format
for a given target. By default all targets `default_target` is `Elf`,
except for the esp32* targets which are now `Idf`.