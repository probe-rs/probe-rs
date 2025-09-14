`FlashProgress` now wraps an `FnMut`.
`FlashProgress` no longer implements `Clone`.
Flasher APIs now take `FlashProgress` by mutable reference.