//! A size-checked Crossterm adapter. Ratatui's inline resize currently clears
//! the screen on horizontal shrink; reject that transition and fall back instead.

use ratatui::{
    backend::{Backend, ClearType, CrosstermBackend, WindowSize},
    buffer::Cell,
    layout::{Position, Size},
};
use std::io;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

pub(super) struct InlineBackend {
    pub inner: CrosstermBackend<io::Stderr>,
    pub size: Size,
    pub invalidated: Arc<AtomicBool>,
}

impl Backend for InlineBackend {
    type Error = io::Error;

    fn draw<'a, I>(&mut self, content: I) -> io::Result<()>
    where
        I: Iterator<Item = (u16, u16, &'a Cell)>,
    {
        self.size()?;
        self.inner.draw(content)
    }
    fn append_lines(&mut self, n: u16) -> io::Result<()> {
        self.size()?;
        self.inner.append_lines(n)
    }
    fn hide_cursor(&mut self) -> io::Result<()> {
        self.inner.hide_cursor()
    }
    fn show_cursor(&mut self) -> io::Result<()> {
        self.inner.show_cursor()
    }
    fn get_cursor_position(&mut self) -> io::Result<Position> {
        self.size()?;
        self.inner.get_cursor_position()
    }
    fn set_cursor_position<P: Into<Position>>(&mut self, position: P) -> io::Result<()> {
        self.size()?;
        self.inner.set_cursor_position(position)
    }
    fn clear(&mut self) -> io::Result<()> {
        Err(io::Error::other("inline output must not clear the screen"))
    }
    fn clear_region(&mut self, kind: ClearType) -> io::Result<()> {
        self.size()?;
        if matches!(kind, ClearType::All | ClearType::BeforeCursor) {
            return self.clear();
        }
        self.inner.clear_region(kind)
    }
    fn size(&self) -> io::Result<Size> {
        if self.invalidated.load(Ordering::Relaxed) || self.inner.size()? != self.size {
            return Err(io::Error::other("inline viewport resized"));
        }
        Ok(self.size)
    }
    fn window_size(&mut self) -> io::Result<WindowSize> {
        self.size()?;
        self.inner.window_size()
    }
    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}
