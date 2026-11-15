use camel::Rect;
use image::RgbImage;

/// Foreground components separate lines inside an independently detected text box.
pub(crate) struct Ink {
    pub bounds: Rect,
    domain: Rect,
    pixels: Vec<bool>,
    drop_caps: Vec<(Rect, Rect)>,
    white_paper: bool,
    bubble_text: bool,
    reading: Vec<(Rect, Rect)>,
}

#[derive(Clone, Copy)]
struct Component {
    bounds: Rect,
    seed: usize,
    has_core: bool,
}

impl Ink {
    pub fn new(image: &RgbImage, bounds: Rect) -> Self {
        Self::with_projection(image, bounds, false)
    }

    pub fn with_luminance(image: &RgbImage, bounds: Rect) -> Self {
        Self::with_projection(image, bounds, true)
    }

    #[expect(
        clippy::many_single_char_names,
        reason = "x/y and r/g/b are conventional pixel coordinate and channel names"
    )]
    fn with_projection(image: &RgbImage, bounds: Rect, force_luminance: bool) -> Self {
        let domain = bounds;
        let margin = (bounds.width.min(bounds.height) / 2).clamp(4, 32);
        let x = bounds.x.saturating_sub(margin);
        let y = bounds.y.saturating_sub(margin);
        let bounds = Rect {
            x,
            y,
            width: bounds.right().saturating_add(margin).min(image.width()) - x,
            height: bounds.bottom().saturating_add(margin).min(image.height()) - y,
        };
        let mut histograms = [[0_u64; 256]; 2];
        let mut paper = 0_u64;
        for y in domain.y..domain.bottom() {
            for x in domain.x..domain.right() {
                let [r, g, b] = image.get_pixel(x, y).0;
                paper += u64::from(r.min(g).min(b) >= 225);
                histograms[0][usize::from(r.max(g).max(b))] += 1;
                histograms[1][usize::from(luminance(r, g, b))] += 1;
            }
        }
        let (maximum, maximum_score) = otsu(&histograms[0]);
        let (luma, luma_score) = otsu(&histograms[1]);
        // Saturated lettering can share its brightest channel with white paper.
        // Use luminance when it separates those classes more clearly. Colored
        // artwork keeps the maximum-channel projection that isolates dark ink.
        let use_luminance = (force_luminance
            || paper * 2 > u64::from(domain.width) * u64::from(domain.height))
            && luma_score > maximum_score;
        let (threshold, histogram) = if use_luminance {
            (luma, &histograms[1])
        } else {
            (maximum, &histograms[0])
        };
        // Estimate paper inside the detector box. The expanded crop's perimeter
        // can coincide with a black caption frame and invert the text polarity.
        let light: u64 = histogram[usize::from(threshold) + 1..].iter().sum();
        let light_background = light * 2 >= histogram.iter().sum();
        let mut pixels = Vec::with_capacity(bounds.width as usize * bounds.height as usize);
        for y in bounds.y..bounds.bottom() {
            for x in bounds.x..bounds.right() {
                let [r, g, b] = image.get_pixel(x, y).0;
                let level = if use_luminance {
                    luminance(r, g, b)
                } else {
                    r.max(g).max(b)
                };
                pixels.push((level <= threshold) == light_background);
            }
        }
        Self {
            bounds,
            domain,
            pixels,
            drop_caps: Vec::new(),
            bubble_text: false,
            reading: Vec::new(),
            white_paper: light_background
                && paper * 2 > u64::from(domain.width) * u64::from(domain.height),
        }
    }

    pub fn in_bubble(mut self) -> Self {
        self.bubble_text = true;
        self
    }

    pub fn has_white_paper(&self) -> bool {
        self.white_paper
    }

    fn contains(&self, x: u32, y: u32) -> bool {
        self.pixels[((y - self.bounds.y) * self.bounds.width + x - self.bounds.x) as usize]
    }

    #[expect(
        clippy::many_single_char_names,
        reason = "x/y and l/r/t/b are conventional pixel-coordinate and edge names"
    )]
    pub fn lines(&mut self) -> Vec<Rect> {
        let width = self.bounds.width as usize;
        let height = self.bounds.height as usize;
        let mut seen = vec![false; self.pixels.len()];
        let mut components = Vec::new();
        let mut extensions = Vec::new();
        let margin = (self.domain.width.min(self.domain.height) / 8).clamp(2, 12);
        let x = self.domain.x.saturating_sub(margin);
        let y = self.domain.y.saturating_sub(margin);
        let core = Rect {
            x,
            y,
            width: self.domain.right().saturating_add(margin) - x,
            height: self.domain.bottom().saturating_add(margin) - y,
        };
        let mut queue = Vec::new();
        for start in 0..seen.len() {
            if seen[start] || !self.pixels[start] {
                continue;
            }
            queue.clear();
            queue.push(start);
            seen[start] = true;
            let (mut l, mut r, mut t, mut b) = (width, 0, height, 0);
            let mut head = 0;
            while head < queue.len() {
                let index = queue[head];
                head += 1;
                let (x, y) = (index % width, index / width);
                l = l.min(x);
                r = r.max(x + 1);
                t = t.min(y);
                b = b.max(y + 1);
                for yy in y.saturating_sub(1)..=(y + 1).min(height - 1) {
                    for xx in x.saturating_sub(1)..=(x + 1).min(width - 1) {
                        let next = yy * width + xx;
                        if !seen[next] && self.pixels[next] {
                            seen[next] = true;
                            queue.push(next);
                        }
                    }
                }
            }
            // Read beyond the detector box so a clipped balloon outline can be
            // identified by its connection to the crop boundary.
            let cx = self.bounds.x + usize::midpoint(l, r) as u32;
            let cy = self.bounds.y + usize::midpoint(t, b) as u32;
            if queue.len() >= 3
                && l > 0
                && t > 0
                && r < width
                && b < height
                && cx >= self.domain.x
                && cx < self.domain.right()
                && cy >= self.domain.y
                && cy < self.domain.bottom()
            {
                let rect = Rect {
                    x: self.bounds.x + l as u32,
                    y: self.bounds.y + t as u32,
                    width: (r - l) as u32,
                    height: (b - t) as u32,
                };
                let has_core = queue.iter().any(|&i| {
                    let (x, y) = (i % width, i / width);
                    self.is_core(i)
                        && (y - 1..=y + 1).any(|yy| {
                            (x - 1..=x + 1).any(|xx| {
                                let next = yy * width + xx;
                                next != i && self.is_core(next)
                            })
                        })
                });
                if rect.x > core.x
                    && rect.y > core.y
                    && rect.right() < core.right()
                    && rect.bottom() < core.bottom()
                {
                    components.push(Component {
                        bounds: rect,
                        seed: start,
                        has_core,
                    });
                } else {
                    extensions.push(Component {
                        bounds: rect,
                        seed: start,
                        has_core,
                    });
                }
            }
        }
        // One common vertical band distinguishes a short row from touching
        // rows or a drop cap alongside multiple baselines.
        let overlap_top = components.iter().map(|c| c.bounds.y).max().unwrap_or(0);
        let overlap_bottom = components
            .iter()
            .map(|c| c.bounds.bottom())
            .min()
            .unwrap_or(0);
        let single_row = self.white_paper && overlap_top < overlap_bottom;
        let mut heights: Vec<_> = components
            .iter()
            .filter(|c| c.bounds.height >= 3 && c.bounds.width >= 2)
            .filter(|c| {
                // Neighboring small marks do not set the type size when they
                // accompany a common band of taller letters.
                let small = c.bounds;
                let center = small.y + small.height / 2;
                !self.white_paper
                    || !components.iter().any(|other| {
                        let body = other.bounds;
                        body.height > small.height * 2
                            && body.y <= center
                            && center < body.bottom()
                            && (small.right() <= body.x || body.right() <= small.x)
                            && horizontal_gap(small, body) <= body.height * 2
                            && (single_row
                                || self.bubble_text
                                    && components.iter().any(|peer| {
                                        let peer = peer.bounds;
                                        peer != body
                                            && peer.height > small.height * 2
                                            && peer.height.min(body.height) * 2
                                                >= peer.height.max(body.height)
                                            && peer.y <= center
                                            && center < peer.bottom()
                                            && horizontal_gap(peer, body) <= body.height * 2
                                    }))
                    })
            })
            .map(|c| c.bounds.height)
            .collect();
        heights.sort_unstable();
        let Some(&typical) = heights.get(heights.len() / 2) else {
            return Vec::new();
        };
        // Hairlines outside the lettering's paper are often illustration details.
        // Thin glyphs remain supported by their paper or a closed letter counter.
        if self.white_paper
            && components
                .iter()
                .any(|c| !c.has_core && c.bounds.height * 2 >= typical)
            && components.iter().any(|c| {
                c.has_core && c.bounds.height * 2 >= typical && c.bounds.height <= typical * 2
            })
        {
            self.mark_supported_paper(&components, typical, &mut seen, &mut queue);
            components.retain(|c| {
                c.has_core
                    || self.has_counter(c.bounds, &mut seen, &mut queue)
                    || self.touches_paper(c.seed, &mut seen, &mut queue)
            });
        }
        components.sort_by_key(|c| c.bounds.x);
        // A closed caption frame surrounds its glyphs but is not a text row.
        let frames: Vec<_> = components
            .iter()
            .filter(|c| self.is_caption_frame(**c, &components, typical))
            .map(|c| c.seed)
            .collect();
        components.retain(|c| !frames.contains(&c.seed));
        let mut lines: Vec<(Rect, f64, usize)> = Vec::new();
        let mut drop_caps = Vec::new();
        let mut seeds = Vec::new();
        for &Component {
            bounds: component,
            seed,
            ..
        } in components
            .iter()
            .filter(|c| c.bounds.height * 2 >= typical && c.bounds.height <= typical * 2)
        {
            seeds.push(seed);
            let center = f64::from(component.y) + f64::from(component.height) / 2.0;
            let nearest = lines
                .iter_mut()
                .filter(|line| horizontal_gap(line.0, component) <= typical * 3)
                .min_by(|a, b| {
                    (a.1 / a.2 as f64 - center)
                        .abs()
                        .total_cmp(&(b.1 / b.2 as f64 - center).abs())
                });
            if let Some(line) = nearest
                .filter(|line| (line.1 / line.2 as f64 - center).abs() < f64::from(typical) * 0.55)
            {
                line.0 = line.0.union(component);
                line.1 += center;
                line.2 += 1;
            } else {
                lines.push((component, center, 1));
            }
        }
        // A caption can combine a large heading with much smaller body text.
        // Recover independent rows of similarly sized large glyphs as a group;
        // a solitary tall component may instead be a frame or a drop cap.
        let mut large_rows: Vec<(Rect, Vec<usize>)> = Vec::new();
        for component in components
            .iter()
            .filter(|c| c.bounds.height > typical * 2 && c.bounds.width <= c.bounds.height * 2)
        {
            let rect = component.bounds;
            let row = large_rows.iter_mut().find(|(row, _)| {
                rect.height.min(row.height) * 4 >= rect.height.max(row.height) * 3
                    && rect.y.abs_diff(row.y) * 4 <= row.height
                    && rect.bottom().abs_diff(row.bottom()) * 4 <= row.height
                    && horizontal_gap(rect, *row) <= row.height
            });
            if let Some((row, members)) = row {
                *row = row.union(rect);
                members.push(component.seed);
            } else {
                large_rows.push((rect, vec![component.seed]));
            }
        }
        for (rect, members) in large_rows
            .into_iter()
            .filter(|(_, members)| members.len() >= 3)
        {
            let center = f64::from(rect.y) + f64::from(rect.height) / 2.0;
            lines.push((rect, center * members.len() as f64, members.len()));
            seeds.extend(members);
        }
        // A low comma can have half the usual glyph height and initially form
        // its own row. Attach it to the body whose vertical span it overlaps.
        let bodies: Vec<_> = lines.iter().map(|line| line.0).collect();
        let joins: Vec<_> = lines
            .iter()
            .enumerate()
            .map(|(i, line)| {
                if line.2 != 1 || line.0.height * 2 > typical {
                    return None;
                }
                bodies
                    .iter()
                    .enumerate()
                    .filter(|&(j, body)| {
                        j != i
                            && (lines[j].2 > 1 || body.height * 2 > typical)
                            && body.height > line.0.height
                            && body.y < line.0.bottom()
                            && line.0.y < body.bottom()
                    })
                    .min_by_key(|(_, body)| {
                        (
                            body.bottom().abs_diff(line.0.bottom()),
                            horizontal_gap(**body, line.0),
                        )
                    })
                    .filter(|(_, body)| horizontal_gap(**body, line.0) <= typical * 2)
                    .map(|(j, _)| j)
            })
            .collect();
        for (i, target) in joins.iter().enumerate() {
            if let Some(j) = target {
                lines[*j].0 = lines[*j].0.union(bodies[i]);
            }
        }
        let bodies: Vec<_> = bodies
            .into_iter()
            .enumerate()
            .filter(|(i, _)| joins[*i].is_none())
            .map(|(_, body)| body)
            .collect();
        let mut index = 0;
        lines.retain(|_| {
            let keep = joins[index].is_none();
            index += 1;
            keep
        });
        // Use the original body for both axes. Added punctuation must not
        // extend the reach of a row or redirect a mark from a closer row.
        for &Component {
            bounds: component,
            seed,
            ..
        } in components.iter().filter(|c| c.bounds.height * 2 < typical)
        {
            if let Some((i, _)) = bodies
                .iter()
                .enumerate()
                .min_by_key(|(_, body)| {
                    (
                        vertical_gap(**body, component),
                        body.bottom().abs_diff(component.bottom()),
                        horizontal_gap(**body, component),
                    )
                })
                .filter(|(_, body)| {
                    horizontal_gap(**body, component) <= typical * 2
                        && body.bottom().abs_diff(component.bottom()) <= typical
                })
            {
                let line = &mut lines[i];
                line.0 = line.0.union(component);
                seeds.push(seed);
            }
        }
        // A clipped initial can be taller than the rest of its line. Extend
        // only an existing line with a complete, baseline-aligned leading glyph.
        for &Component {
            bounds: component,
            seed,
            ..
        } in extensions.iter().filter(|c| c.bounds.height > typical * 2)
        {
            let mut anchors = lines
                .iter()
                .enumerate()
                .filter(|(_, line)| component.y < line.0.bottom() && line.0.y < component.bottom())
                .map(|(i, _)| i);
            if let (Some(index), None) = (anchors.next(), anchors.next()) {
                let line = &mut lines[index];
                if component.x + component.width / 2 < line.0.x
                    && horizontal_gap(line.0, component) <= typical
                    && component.bottom().abs_diff(line.0.bottom()) <= typical / 3
                {
                    line.0 = line.0.union(component);
                    seeds.push(seed);
                }
            }
        }
        // Bold or italic glyphs can touch across rows. Split that component at
        // neighboring line midpoints instead of discarding all of its letters.
        for &Component {
            bounds: component,
            seed,
            ..
        } in components.iter().filter(|c| c.bounds.height > typical * 2)
        {
            if seeds.contains(&seed) {
                continue;
            }
            let mut anchors: Vec<_> = lines
                .iter()
                .enumerate()
                .filter(|(_, line)| {
                    horizontal_gap(line.0, component) <= typical * 3
                        && line.0.y < component.bottom()
                        && component.y < line.0.bottom()
                })
                .map(|(i, line)| (i, line.1 / line.2 as f64))
                .collect();
            anchors.sort_by(|a, b| a.1.total_cmp(&b.1));
            let (Some(&first), Some(&last)) = (anchors.first(), anchors.last()) else {
                continue;
            };
            // A drop cap precedes distinct rows of body text. Character
            // fragments at different heights are not separate text rows.
            if anchors.len() >= 2
                && anchors
                    .windows(2)
                    .all(|pair| lines[pair[0].0].0.bottom() <= lines[pair[1].0].0.y)
                && component.y.abs_diff(lines[first.0].0.y) <= typical / 3
                && component.bottom().abs_diff(lines[last.0].0.bottom()) <= typical / 3
                && anchors.iter().all(|&(i, _)| {
                    lines[i].2 >= 2
                        && component.right() <= lines[i].0.x
                        && horizontal_gap(component, lines[i].0) <= typical
                })
            {
                seeds.push(seed);
                drop_caps.push((first.0, component));
                continue;
            }
            if last.1 - first.1 < f64::from(typical)
                || component.y.saturating_add(typical) < lines[first.0].0.y
                || component.bottom() > lines[last.0].0.bottom().saturating_add(typical)
            {
                continue;
            }
            seeds.push(seed);
            let mut top = component.y;
            for (i, &(index, center)) in anchors.iter().enumerate() {
                let bottom = anchors.get(i + 1).map_or(component.bottom(), |next| {
                    f64::midpoint(center, next.1) as u32
                });
                if let Some(part) = self.tighten(Rect {
                    y: top,
                    height: bottom.saturating_sub(top),
                    ..component
                }) {
                    lines[index].0 = lines[index].0.union(part);
                }
                top = bottom;
            }
        }
        // Word bounds and CTC separators use the selected glyphs. A line's
        // rectangle can also contain artwork above shorter neighboring letters.
        seen.fill(false);
        queue.clear();
        for seed in seeds {
            seen[seed] = true;
            queue.push(seed);
        }
        let mut head = 0;
        while head < queue.len() {
            let index = queue[head];
            head += 1;
            let (x, y) = (index % width, index / width);
            for yy in y.saturating_sub(1)..=(y + 1).min(height - 1) {
                for xx in x.saturating_sub(1)..=(x + 1).min(width - 1) {
                    let next = yy * width + xx;
                    if self.pixels[next] && !seen[next] {
                        seen[next] = true;
                        queue.push(next);
                    }
                }
            }
        }
        self.reading.clear();
        if self.bubble_text && self.white_paper {
            for (line, _, _) in &lines {
                if drop_caps.iter().any(|(i, _)| lines[*i].0 == *line) {
                    continue;
                }
                let members: Vec<_> = components
                    .iter()
                    .map(|c| c.bounds)
                    .filter(|c| {
                        c.x >= line.x
                            && c.right() <= line.right()
                            && c.y >= line.y
                            && c.bottom() <= line.bottom()
                    })
                    .collect();
                let tall: Vec<_> = members
                    .iter()
                    .copied()
                    .filter(|c| c.height * 4 >= typical * 3)
                    .collect();
                if tall.len() < 2 {
                    continue;
                }
                let mut body = tall.into_iter().reduce(Rect::union).unwrap();
                for component in &members {
                    if component.height * 2 >= typical
                        && component.x < body.right()
                        && body.x < component.right()
                    {
                        body = body.union(*component);
                    }
                }
                let left: Vec<_> = members
                    .iter()
                    .copied()
                    .filter(|c| c.right() <= body.x)
                    .collect();
                let right: Vec<_> = members
                    .iter()
                    .copied()
                    .filter(|c| c.x >= body.right())
                    .collect();
                let rays = |side: &[Rect]| {
                    side.len() >= 2
                        && side.iter().all(|c| c.height * 2 < typical)
                        && side.iter().any(|c| c.width >= c.height * 2)
                        && side.iter().map(|c| c.y).max().unwrap() < line.bottom()
                        && side.iter().map(|c| c.y).max().unwrap()
                            >= side.iter().map(|c| c.bottom()).min().unwrap()
                };
                if rays(&left) && rays(&right) {
                    self.reading.push((
                        *line,
                        Rect {
                            x: body.x,
                            y: line.y,
                            width: body.width,
                            height: line.height,
                        },
                    ));
                }
            }
        }
        self.pixels = seen;
        self.drop_caps = drop_caps
            .into_iter()
            .map(|(i, cap)| (lines[i].0, cap))
            .collect();
        lines.sort_by_key(|line| (line.0.y, line.0.x));
        lines.into_iter().map(|line| line.0).collect()
    }

    pub fn drop_cap(&self, line: Rect) -> Option<Rect> {
        self.drop_caps
            .iter()
            .find(|(body, _)| *body == line)
            .map(|(_, cap)| *cap)
    }

    fn is_core(&self, index: usize) -> bool {
        let w = self.bounds.width as usize;
        let h = self.bounds.height as usize;
        let (x, y) = (index % w, index / w);
        x > 0
            && y > 0
            && x + 1 < w
            && y + 1 < h
            && self.pixels[index]
            && self.pixels[index - 1]
            && self.pixels[index + 1]
            && self.pixels[index - w]
            && self.pixels[index + w]
    }

    fn flood_background(&self, seen: &mut [bool], queue: &mut Vec<usize>) {
        let width = self.bounds.width as usize;
        let height = self.bounds.height as usize;
        let mut head = 0;
        while head < queue.len() {
            let i = queue[head];
            head += 1;
            let (x, y) = (i % width, i / width);
            for next in [
                x.checked_sub(1).map(|x| y * width + x),
                (x + 1 < width).then_some(i + 1),
                y.checked_sub(1).map(|y| y * width + x),
                (y + 1 < height).then_some(i + width),
            ]
            .into_iter()
            .flatten()
            {
                if !seen[next] && !self.pixels[next] {
                    seen[next] = true;
                    queue.push(next);
                }
            }
        }
    }

    fn mark_supported_paper(
        &self,
        components: &[Component],
        typical: u32,
        seen: &mut [bool],
        queue: &mut Vec<usize>,
    ) {
        let width = self.bounds.width as usize;
        seen.fill(false);
        let mut best = None;
        let mut best_area = 0;
        for seed in 0..seen.len() {
            if seen[seed] || self.pixels[seed] {
                continue;
            }
            seen[seed] = true;
            queue.clear();
            queue.push(seed);
            self.flood_background(seen, queue);
            let area = queue
                .iter()
                .filter(|&&i| {
                    let x = self.bounds.x + (i % width) as u32;
                    let y = self.bounds.y + (i / width) as u32;
                    x >= self.domain.x
                        && x < self.domain.right()
                        && y >= self.domain.y
                        && y < self.domain.bottom()
                })
                .count();
            if area > best_area {
                best = Some(seed);
                best_area = area;
            }
        }
        seen.fill(false);
        queue.clear();
        if let Some(seed) = best {
            seen[seed] = true;
            queue.push(seed);
        }
        for c in components.iter().filter(|c| {
            c.has_core && c.bounds.height * 2 >= typical && c.bounds.height <= typical * 2
        }) {
            // Include counters inside a thick outline, such as the paper around
            // a detached thin letter inside a registered symbol.
            for y in c.bounds.y..c.bounds.bottom() {
                for x in c.bounds.x..c.bounds.right() {
                    let i = ((y - self.bounds.y) * self.bounds.width + x - self.bounds.x) as usize;
                    if !self.pixels[i] && !seen[i] {
                        seen[i] = true;
                        queue.push(i);
                    }
                }
            }
            for i in [c.seed - 1, c.seed + 1, c.seed - width, c.seed + width] {
                if !self.pixels[i] && !seen[i] {
                    seen[i] = true;
                    queue.push(i);
                }
            }
        }
        self.flood_background(seen, queue);
    }

    fn touches_paper(&self, start: usize, seen: &mut [bool], queue: &mut Vec<usize>) -> bool {
        let width = self.bounds.width as usize;
        let height = self.bounds.height as usize;
        queue.clear();
        queue.push(start);
        seen[start] = true;
        let mut head = 0;
        while head < queue.len() {
            let i = queue[head];
            head += 1;
            let (x, y) = (i % width, i / width);
            for yy in y.saturating_sub(1)..=(y + 1).min(height - 1) {
                for xx in x.saturating_sub(1)..=(x + 1).min(width - 1) {
                    let next = yy * width + xx;
                    if !self.pixels[next] && seen[next] {
                        return true;
                    }
                    if self.pixels[next] && !seen[next] {
                        seen[next] = true;
                        queue.push(next);
                    }
                }
            }
        }
        false
    }

    fn has_counter(&self, bounds: Rect, seen: &mut [bool], queue: &mut Vec<usize>) -> bool {
        let width = bounds.width as usize;
        let height = bounds.height as usize;
        if width < 3 || height < 3 {
            return false;
        }
        let mut mask: Vec<u8> = (bounds.y..bounds.bottom())
            .flat_map(|y| (bounds.x..bounds.right()).map(move |x| u8::from(self.contains(x, y))))
            .collect();
        queue.clear();
        for y in 0..height {
            for x in 0..width {
                let i = y * width + x;
                if (x == 0 || y == 0 || x + 1 == width || y + 1 == height) && mask[i] == 0 {
                    mask[i] = 2;
                    queue.push(i);
                }
            }
        }
        flood_paper(&mut mask, width, queue, 2);
        if !mask.contains(&0) {
            return false;
        }
        // Enclosed paper also supports detached strokes inside symbols such as ©.
        queue.clear();
        for (i, &v) in mask.iter().enumerate() {
            if v == 0 {
                let x = bounds.x + (i % width) as u32;
                let y = bounds.y + (i / width) as u32;
                let index = ((y - self.bounds.y) * self.bounds.width + x - self.bounds.x) as usize;
                if !seen[index] {
                    seen[index] = true;
                    queue.push(index);
                }
            }
        }
        self.flood_background(seen, queue);
        true
    }

    pub fn recognition_bounds(&self, line: Rect) -> Rect {
        self.reading
            .iter()
            .find(|(source, _)| *source == line)
            .map_or(line, |(_, crop)| *crop)
    }

    /// Return the horizontal offset for mapping CTC intervals back to source pixels.
    pub fn recognition_crop(&self, image: &RgbImage, line: Rect) -> (RgbImage, i64) {
        let crop = self.recognition_bounds(line);
        let body =
            image::imageops::crop_imm(image, crop.x, crop.y, crop.width, crop.height).to_image();
        let Some(cap) = self.drop_cap(line) else {
            return (body, i64::from(line.x) - i64::from(crop.x));
        };
        let width =
            (u64::from(cap.width) * u64::from(line.height) / u64::from(cap.height)).max(1) as u32;
        let gap = line.x - cap.right();
        let offset = width + gap;
        let mut output = RgbImage::new(offset + line.width, line.height);
        let glyph =
            image::imageops::crop_imm(image, cap.x, cap.y, cap.width, cap.height).to_image();
        let glyph = image::imageops::resize(
            &glyph,
            width,
            line.height,
            image::imageops::FilterType::Triangle,
        );
        image::imageops::replace(&mut output, &glyph, 0, 0);
        if gap > 0 {
            let space =
                image::imageops::crop_imm(image, cap.right(), line.y, gap, line.height).to_image();
            image::imageops::replace(&mut output, &space, i64::from(width), 0);
        }
        image::imageops::replace(&mut output, &body, i64::from(offset), 0);
        (output, i64::from(offset))
    }

    fn is_caption_frame(
        &self,
        component: Component,
        components: &[Component],
        typical: u32,
    ) -> bool {
        let bounds = component.bounds;
        let first = components.partition_point(|c| c.bounds.x <= bounds.x);
        let children: Vec<_> = components[first..]
            .iter()
            .take_while(|c| c.bounds.x < bounds.right())
            .filter(|c| {
                c.bounds.right() < bounds.right()
                    && c.bounds.y > bounds.y
                    && c.bounds.bottom() < bounds.bottom()
                    && c.bounds.height * 2 >= typical
                    && c.bounds.height <= typical * 2
            })
            .collect();
        if children.len() < 2 {
            return false;
        }
        let width = bounds.width as usize;
        let height = bounds.height as usize;
        let local = |seed: usize| {
            let x = self.bounds.x + (seed % self.bounds.width as usize) as u32;
            let y = self.bounds.y + (seed / self.bounds.width as usize) as u32;
            (y - bounds.y) as usize * width + (x - bounds.x) as usize
        };
        // 1: boundary ink; 2: visited exterior; 3: current enclosed paper.
        let mut mask = vec![0_u8; width * height];
        let mut queue = vec![local(component.seed)];
        mask[queue[0]] = 1;
        let mut head = 0;
        while head < queue.len() {
            let index = queue[head];
            head += 1;
            let (x, y) = (index % width, index / width);
            for ny in y.saturating_sub(1)..=(y + 1).min(height - 1) {
                for nx in x.saturating_sub(1)..=(x + 1).min(width - 1) {
                    let next = ny * width + nx;
                    if mask[next] == 0 && self.contains(bounds.x + nx as u32, bounds.y + ny as u32)
                    {
                        mask[next] = 1;
                        queue.push(next);
                    }
                }
            }
        }
        queue.clear();
        for y in 0..height {
            for x in 0..width {
                let index = y * width + x;
                if (x == 0 || y == 0 || x + 1 == width || y + 1 == height) && mask[index] == 0 {
                    mask[index] = 2;
                    queue.push(index);
                }
            }
        }
        flood_paper(&mut mask, width, &mut queue, 2);
        // The glyphs must share one enclosed paper surface. Separate counters
        // inside connected outlined letters do not form a caption frame.
        for child in &children {
            let seed = local(child.seed);
            if mask[seed] != 0 {
                continue;
            }
            queue.clear();
            queue.push(seed);
            mask[seed] = 3;
            flood_paper(&mut mask, width, &mut queue, 3);
            if children
                .iter()
                .filter(|c| mask[local(c.seed)] == 3)
                .take(2)
                .count()
                == 2
            {
                let (mut left, mut right, mut top, mut bottom) = (width, 0, height, 0);
                for &index in &queue {
                    let (x, y) = (index % width, index / width);
                    left = left.min(x);
                    right = right.max(x);
                    top = top.min(y);
                    bottom = bottom.max(y);
                }
                let rectangle = (right - left + 1) * (bottom - top + 1);
                if queue.len() * 100 >= rectangle * 96 {
                    return true;
                }
            }
            for &index in &queue {
                mask[index] = 2;
            }
        }
        false
    }

    pub fn tighten(&self, bounds: Rect) -> Option<Rect> {
        let mut left = bounds.right();
        let mut right = bounds.x;
        let mut top = bounds.bottom();
        let mut bottom = bounds.y;
        for y in bounds.y..bounds.bottom() {
            for x in bounds.x..bounds.right() {
                if self.contains(x, y) {
                    left = left.min(x);
                    right = right.max(x + 1);
                    top = top.min(y);
                    bottom = bottom.max(y + 1);
                }
            }
        }
        (right > left && bottom > top).then_some(Rect {
            x: left,
            y: top,
            width: right.saturating_sub(left),
            height: bottom.saturating_sub(top),
        })
    }

    /// Align a CTC word separator with nearby source whitespace.
    pub fn separator(&self, line: Rect, estimate: u32, minimum: u32) -> u32 {
        let radius = line.height / 2;
        let left = estimate.saturating_sub(radius).max(minimum).max(line.x);
        let right = estimate.saturating_add(radius).min(line.right());
        (left..right)
            .min_by_key(|&x| {
                let ink = (line.y..line.bottom())
                    .filter(|&y| self.contains(x, y))
                    .count();
                (ink, x.abs_diff(estimate))
            })
            .unwrap_or(estimate.clamp(minimum.min(line.right()), line.right()))
    }
}

fn flood_paper(mask: &mut [u8], width: usize, queue: &mut Vec<usize>, label: u8) {
    let height = mask.len() / width;
    let mut head = 0;
    while head < queue.len() {
        let index = queue[head];
        head += 1;
        let (x, y) = (index % width, index / width);
        for next in [
            x.checked_sub(1).map(|x| y * width + x),
            (x + 1 < width).then_some(index + 1),
            y.checked_sub(1).map(|y| y * width + x),
            (y + 1 < height).then_some(index + width),
        ]
        .into_iter()
        .flatten()
        {
            if mask[next] == 0 {
                mask[next] = label;
                queue.push(next);
            }
        }
    }
}

fn horizontal_gap(a: Rect, b: Rect) -> u32 {
    a.x.saturating_sub(b.right())
        .max(b.x.saturating_sub(a.right()))
}

fn vertical_gap(a: Rect, b: Rect) -> u32 {
    a.y.saturating_sub(b.bottom())
        .max(b.y.saturating_sub(a.bottom()))
}

fn luminance(r: u8, g: u8, b: u8) -> u8 {
    ((77 * u32::from(r) + 150 * u32::from(g) + 29 * u32::from(b) + 128) >> 8) as u8
}

fn otsu(histogram: &[u64; 256]) -> (u8, f64) {
    let total: u64 = histogram.iter().sum();
    let sum: u64 = histogram
        .iter()
        .enumerate()
        .map(|(i, &n)| i as u64 * n)
        .sum();
    let (mut count, mut partial) = (0, 0);
    let (mut best, mut threshold) = (0.0, 0);
    for (level, &n) in histogram.iter().enumerate() {
        count += n;
        partial += n * level as u64;
        if count == 0 || count == total {
            continue;
        }
        let difference =
            partial as f64 / count as f64 - (sum - partial) as f64 / (total - count) as f64;
        let variance = count as f64 * (total - count) as f64 * difference.powi(2);
        if variance > best {
            best = variance;
            threshold = level as u8;
        }
    }
    (threshold, best)
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::Rgb;

    #[test]
    fn mixed_type_sizes_keep_both_heading_and_body_rows() {
        let image = RgbImage::from_fn(160, 170, |x, y| {
            let heading = [15, 55, 95].into_iter().any(|top| {
                (top..top + 30).contains(&y)
                    && [20, 45, 70, 95]
                        .into_iter()
                        .any(|left| (left..left + 15).contains(&x))
            });
            let body =
                (140..150).contains(&y) && (0..14).any(|n| (20 + n * 8..25 + n * 8).contains(&x));
            Rgb([if heading || body { 0 } else { 255 }; 3])
        });
        let mut ink = Ink::new(
            &image,
            Rect {
                x: 10,
                y: 5,
                width: 140,
                height: 155,
            },
        );
        let rows = ink.lines();
        assert_eq!(rows.len(), 4, "{rows:?}");
        assert_eq!(
            rows.iter().map(|r| r.y).collect::<Vec<_>>(),
            [15, 55, 95, 140]
        );
    }

    #[test]
    fn punctuation_majority_keeps_the_short_utterances_letter() {
        for mark_y in [28, 37] {
            let image = RgbImage::from_fn(110, 70, |x, y| {
                let letter = (20..26).contains(&x) && (20..40).contains(&y);
                let marks = (mark_y..mark_y + 4).contains(&y)
                    && [35, 44, 53]
                        .into_iter()
                        .any(|left| (left..left + 4).contains(&x));
                Rgb([if letter || marks { 0 } else { 255 }; 3])
            });
            let mut ink = Ink::new(
                &image,
                Rect {
                    x: 10,
                    y: 10,
                    width: 90,
                    height: 50,
                },
            );
            assert_eq!(
                ink.lines(),
                [Rect {
                    x: 20,
                    y: 20,
                    width: 37,
                    height: 20.max(mark_y + 4 - 20)
                }]
            );
            assert!(ink.contains(22, 20));
            assert!(ink.contains(54, mark_y));
        }
    }

    #[test]
    fn aligned_letters_outweigh_marks_above_and_below_their_band() {
        let image = RgbImage::from_fn(110, 70, |x, y| {
            let letters = (20..40).contains(&y)
                && [35, 47, 59]
                    .into_iter()
                    .any(|left| (left..left + 6).contains(&x));
            let rays = [18, 28, 38]
                .into_iter()
                .any(|top| (top..top + 4).contains(&y))
                && ((20..27).contains(&x) || (73..80).contains(&x));
            Rgb([if letters || rays { 0 } else { 255 }; 3])
        });
        let mut ink = Ink::new(
            &image,
            Rect {
                x: 10,
                y: 10,
                width: 85,
                height: 45,
            },
        )
        .in_bubble();
        let lines = ink.lines();
        assert_eq!(lines.len(), 1);
        for x in [35, 47, 59] {
            assert!(ink.contains(x, 20));
        }
    }

    #[test]
    fn punctuation_follows_body_spans_without_chaining_between_rows() {
        let image = RgbImage::from_fn(130, 110, |x, y| {
            let upper = (20..40).contains(&y)
                && [20, 35, 50]
                    .into_iter()
                    .any(|left| (left..left + 5).contains(&x));
            let lower = (50..70).contains(&y)
                && [20, 35]
                    .into_iter()
                    .any(|left| (left..left + 5).contains(&x));
            let comma = (56..60).contains(&x) && (38..48).contains(&y);
            let first_speck = (88..90).contains(&x) && (55..58).contains(&y);
            let next_speck = (96..98).contains(&x) && (74..77).contains(&y);
            Rgb([if upper || lower || comma || first_speck || next_speck {
                0
            } else {
                255
            }; 3])
        });
        let mut ink = Ink::new(
            &image,
            Rect {
                x: 10,
                y: 10,
                width: 110,
                height: 80,
            },
        );
        assert_eq!(
            ink.lines(),
            [
                Rect {
                    x: 20,
                    y: 20,
                    width: 40,
                    height: 28
                },
                Rect {
                    x: 20,
                    y: 50,
                    width: 20,
                    height: 20
                }
            ]
        );
        assert!(ink.contains(56, 47));
        assert!(!ink.contains(88, 55));
        assert!(!ink.contains(96, 74));
    }

    #[test]
    fn punctuation_chooses_the_nearby_run_on_a_shared_baseline() {
        let image = RgbImage::from_fn(260, 90, |x, y| {
            let letters = (20..40).contains(&y)
                && [20, 35, 180, 195]
                    .into_iter()
                    .any(|left| (left..left + 5).contains(&x));
            let period = (205..208).contains(&x) && (38..41).contains(&y);
            Rgb([if letters || period { 0 } else { 255 }; 3])
        });
        let mut ink = Ink::new(
            &image,
            Rect {
                x: 10,
                y: 10,
                width: 225,
                height: 55,
            },
        );
        assert_eq!(
            ink.lines(),
            [
                Rect {
                    x: 20,
                    y: 20,
                    width: 20,
                    height: 20
                },
                Rect {
                    x: 180,
                    y: 20,
                    width: 28,
                    height: 21
                }
            ]
        );
        assert!(ink.contains(205, 40));
    }

    #[test]
    fn descending_comma_stays_with_the_upper_baseline_when_rows_overlap() {
        let image = RgbImage::from_fn(120, 100, |x, y| {
            let upper = (20..42).contains(&y)
                && [20, 35, 50]
                    .into_iter()
                    .any(|left| (left..left + 5).contains(&x));
            let lower = (45..69).contains(&y)
                && [20, 35, 50, 65]
                    .into_iter()
                    .any(|left| (left..left + 5).contains(&x));
            let comma = (59..63).contains(&x) && (37..46).contains(&y);
            Rgb([if upper || lower || comma { 0 } else { 255 }; 3])
        });
        let mut ink = Ink::new(
            &image,
            Rect {
                x: 10,
                y: 10,
                width: 90,
                height: 70,
            },
        );
        assert_eq!(
            ink.lines(),
            [
                Rect {
                    x: 20,
                    y: 20,
                    width: 43,
                    height: 26
                },
                Rect {
                    x: 20,
                    y: 45,
                    width: 50,
                    height: 24
                }
            ]
        );
        assert!(ink.contains(59, 45));
    }

    #[test]
    fn detached_small_text_keeps_its_own_row() {
        let image = RgbImage::from_fn(180, 110, |x, y| {
            let normal = (20..40).contains(&y)
                && [20, 35, 50]
                    .into_iter()
                    .any(|left| (left..left + 5).contains(&x));
            let small = (130..134).contains(&x) && (70..80).contains(&y);
            Rgb([if normal || small { 0 } else { 255 }; 3])
        });
        let mut ink = Ink::new(
            &image,
            Rect {
                x: 10,
                y: 10,
                width: 150,
                height: 85,
            },
        );
        assert_eq!(
            ink.lines(),
            [
                Rect {
                    x: 20,
                    y: 20,
                    width: 35,
                    height: 20
                },
                Rect {
                    x: 130,
                    y: 70,
                    width: 4,
                    height: 10
                }
            ]
        );
    }

    #[test]
    fn clipped_initial_does_not_absorb_a_shape_spanning_staggered_rows() {
        let mut image = RgbImage::from_pixel(120, 90, Rgb([255; 3]));
        for y in 15..45 {
            for x in 18..23 {
                image.put_pixel(x, y, Rgb([0; 3]));
            }
        }
        for y in (15..20).chain(27..32) {
            for x in 23..33 {
                image.put_pixel(x, y, Rgb([0; 3]));
            }
        }
        for y in 15..25 {
            for x in 75..81 {
                image.put_pixel(x, y, Rgb([0; 3]));
            }
        }
        for y in 35..45 {
            for x in (38..44).chain(51..57).chain(64..70) {
                image.put_pixel(x, y, Rgb([0; 3]));
            }
        }
        let mut ink = Ink::new(
            &image,
            Rect {
                x: 25,
                y: 10,
                width: 60,
                height: 50,
            },
        );
        assert_eq!(
            ink.lines(),
            [
                Rect {
                    x: 75,
                    y: 15,
                    width: 6,
                    height: 10
                },
                Rect {
                    x: 38,
                    y: 35,
                    width: 32,
                    height: 10
                }
            ]
        );
        assert!(!ink.contains(18, 15));
    }

    #[test]
    fn paper_support_keeps_thin_letters_counters_and_neighboring_bubbles() {
        let image = RgbImage::from_fn(140, 120, |x, y| {
            let main_letters = (20..35).contains(&y)
                && ((25..29).contains(&x) || (40..44).contains(&x) || x == 60);
            let punctuation = (68..70).contains(&x) && (33..35).contains(&y);
            let divider = (70..73).contains(&y);
            let bubble = (95..135).contains(&x) && (76..116).contains(&y);
            let bubble_edge = bubble && (!(97..133).contains(&x) || !(78..114).contains(&y));
            let second_letters = (84..99).contains(&y) && ((103..107).contains(&x) || x == 119);
            let art = (84..99).contains(&y) && (x == 25 || x == 31);
            let ring = (55..69).contains(&x)
                && (84..99).contains(&y)
                && (x == 55 || x == 68 || y == 84 || y == 98);
            let inner_c =
                (59..65).contains(&x) && (88..95).contains(&y) && (x == 59 || y == 88 || y == 94);
            Rgb(
                if main_letters
                    || punctuation
                    || divider
                    || bubble_edge
                    || second_letters
                    || art
                    || ring
                    || inner_c
                {
                    [0; 3]
                } else if y < 70 || bubble {
                    [255; 3]
                } else {
                    [240, 170, 170]
                },
            )
        });
        let mut ink = Ink::new(
            &image,
            Rect {
                x: 10,
                y: 10,
                width: 120,
                height: 105,
            },
        );
        assert_eq!(ink.lines().len(), 2);
        for (x, y) in [
            (25, 20),
            (60, 20),
            (68, 33),
            (103, 84),
            (119, 84),
            (55, 84),
            (59, 88),
        ] {
            assert!(ink.contains(x, y), "letter or punctuation lost at {x},{y}");
        }
        for (x, y) in [(25, 84), (31, 84)] {
            assert!(!ink.contains(x, y), "outside hairline retained at {x},{y}");
        }
    }

    #[test]
    fn uniformly_thin_lettering_does_not_require_a_thicker_anchor() {
        let image = RgbImage::from_fn(100, 70, |x, y| {
            let letter = [20, 35, 50].into_iter().any(|left| {
                (left..left + 9).contains(&x)
                    && (20..35).contains(&y)
                    && (x == left || x == left + 8 || y == 27)
            });
            Rgb([if letter { 0 } else { 255 }; 3])
        });
        let mut ink = Ink::new(
            &image,
            Rect {
                x: 15,
                y: 15,
                width: 50,
                height: 25,
            },
        );
        assert_eq!(
            ink.lines(),
            [Rect {
                x: 20,
                y: 20,
                width: 39,
                height: 15
            }]
        );
        for x in [20, 35, 50] {
            assert!(ink.contains(x, 20));
            assert!(ink.contains(x + 4, 27));
        }
    }

    #[test]
    fn symbols_on_supported_paper_keep_detached_thin_inner_letters() {
        for thickness in [1, 3] {
            let image = RgbImage::from_fn(120, 70, |x, y| {
                let ring = (70..93).contains(&x)
                    && (20..43).contains(&y)
                    && (!(70 + thickness..93 - thickness).contains(&x)
                        || !(20 + thickness..43 - thickness).contains(&y));
                let inner = (76..86).contains(&x)
                    && (26..37).contains(&y)
                    && (x == 76 || y == 26 || y == 36);
                let body = (20..25).contains(&x) && (20..35).contains(&y);
                Rgb([if ring || inner || body { 0 } else { 255 }; 3])
            });
            let mut ink = Ink::new(
                &image,
                Rect {
                    x: 10,
                    y: 10,
                    width: 95,
                    height: 45,
                },
            );
            ink.lines();
            assert!(ink.contains(70, 20), "symbol outline lost");
            assert!(
                ink.contains(76, 30),
                "thin inner letter lost inside {thickness}px outline"
            );
        }
    }

    #[test]
    fn segmentation_tracks_coordinates_and_foreground_polarity() {
        for (paper, text) in [
            ([240; 3], [20; 3]),
            ([20; 3], [240; 3]),
            ([255; 3], [255, 40, 0]),
            ([255; 3], [0, 180, 255]),
            ([225; 3], [255, 0, 120]),
            ([35, 120, 190], [5; 3]),
        ] {
            let mut image = RgbImage::from_pixel(80, 60, Rgb(paper));
            for y in (15..25).chain(35..45) {
                for x in (20..30).chain(38..48) {
                    image.put_pixel(x, y, Rgb(text));
                }
            }
            let mut ink = Ink::new(
                &image,
                Rect {
                    x: 10,
                    y: 10,
                    width: 50,
                    height: 40,
                },
            );
            let lines = ink.lines();
            assert_eq!(lines.len(), 2);
            assert_eq!(
                lines[0],
                Rect {
                    x: 20,
                    y: 15,
                    width: 28,
                    height: 10
                }
            );
            assert_eq!(ink.separator(lines[0], 38, 20), 37);
            assert_eq!(
                ink.tighten(Rect {
                    x: 30,
                    y: 15,
                    width: 8,
                    height: 10
                }),
                None
            );
        }
    }

    #[test]
    fn staggered_lines_do_not_require_an_empty_horizontal_row() {
        let mut image = RgbImage::from_pixel(80, 60, Rgb([255; 3]));
        for (x, y) in [(10, 15), (25, 17), (45, 24), (60, 26)] {
            for yy in y..y + 10 {
                for xx in x..x + 7 {
                    image.put_pixel(xx, yy, Rgb([0; 3]));
                }
            }
        }
        let mut ink = Ink::new(
            &image,
            Rect {
                x: 0,
                y: 0,
                width: 80,
                height: 60,
            },
        );
        let lines = ink.lines();
        assert_eq!(lines.len(), 2);
        assert_eq!(
            lines[0],
            Rect {
                x: 10,
                y: 15,
                width: 22,
                height: 12
            }
        );
        assert_eq!(
            lines[1],
            Rect {
                x: 45,
                y: 24,
                width: 22,
                height: 12
            }
        );
    }

    #[test]
    fn distant_ink_does_not_expand_a_line_across_the_page() {
        let mut image = RgbImage::from_pixel(1000, 80, Rgb([255; 3]));
        for (x, y, width, height) in [
            (20, 20, 8, 12),
            (35, 21, 8, 12),
            (500, 22, 7, 12),
            (950, 30, 3, 3),
        ] {
            for yy in y..y + height {
                for xx in x..x + width {
                    image.put_pixel(xx, yy, Rgb([0; 3]));
                }
            }
        }
        let mut ink = Ink::new(
            &image,
            Rect {
                x: 0,
                y: 0,
                width: 1000,
                height: 80,
            },
        );
        let lines = ink.lines();
        assert_eq!(lines.len(), 2);
        assert_eq!(
            lines[0],
            Rect {
                x: 20,
                y: 20,
                width: 23,
                height: 13
            }
        );
        assert_eq!(
            lines[1],
            Rect {
                x: 500,
                y: 22,
                width: 7,
                height: 12
            }
        );
    }

    #[test]
    fn touching_glyphs_between_lines_are_split_instead_of_dropped() {
        let mut image = RgbImage::from_pixel(100, 70, Rgb([255; 3]));
        for y in (20..30).chain(33..43) {
            for x in (15..20).chain(26..31).chain(37..42).chain(48..53) {
                image.put_pixel(x, y, Rgb([0; 3]));
            }
        }
        for y in 29..34 {
            image.put_pixel(50, y, Rgb([0; 3]));
        }
        let mut ink = Ink::new(
            &image,
            Rect {
                x: 5,
                y: 10,
                width: 60,
                height: 45,
            },
        );
        let lines = ink.lines();
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0].right(), 53);
        assert_eq!(lines[1].right(), 53);
        assert!(lines[0].bottom() <= lines[1].y);
        assert_eq!(lines[0].y, 20);
        assert_eq!(lines[1].bottom(), 43);
    }

    #[test]
    fn drop_cap_belongs_to_the_first_of_two_real_text_rows() {
        for (paper, letter) in [([255; 3], [0; 3]), ([20, 80, 130], [255; 3])] {
            for letters_per_row in [1, 3] {
                let mut image = RgbImage::from_pixel(100, 70, Rgb(paper));
                for y in 20..46 {
                    for x in 20..34 {
                        image.put_pixel(x, y, Rgb(letter));
                    }
                }
                for top in [20, 36] {
                    for n in 0..letters_per_row {
                        for y in top..top + 10 {
                            for x in 40 + n * 10..45 + n * 10 {
                                image.put_pixel(x, y, Rgb(letter));
                            }
                        }
                    }
                }
                let original = image.clone();
                let mut ink = Ink::new(
                    &image,
                    Rect {
                        x: 15,
                        y: 15,
                        width: 70,
                        height: 40,
                    },
                );
                let lines = ink.lines();
                if letters_per_row == 1 {
                    assert!(lines.iter().all(|&line| ink.drop_cap(line).is_none()));
                    continue;
                }
                assert_eq!(lines.len(), 2);
                assert_eq!(
                    lines[0],
                    Rect {
                        x: 40,
                        y: 20,
                        width: 25,
                        height: 10
                    }
                );
                assert_eq!(
                    lines[1],
                    Rect {
                        x: 40,
                        y: 36,
                        width: 25,
                        height: 10
                    }
                );
                assert_eq!(
                    ink.drop_cap(lines[0]),
                    Some(Rect {
                        x: 20,
                        y: 20,
                        width: 14,
                        height: 26
                    })
                );
                assert!(ink.drop_cap(lines[1]).is_none());
                let (crop, offset) = ink.recognition_crop(&image, lines[0]);
                assert_eq!(offset, 11);
                let offset = u32::try_from(offset).unwrap();
                assert_eq!(crop.dimensions(), (36, 10));
                for y in 0..10 {
                    for x in 0..25 {
                        assert_eq!(
                            crop.get_pixel(offset + x, y),
                            image.get_pixel(40 + x, 20 + y)
                        );
                    }
                }
                assert_eq!(image, original);
                assert_eq!(
                    ink.tighten(Rect {
                        x: 20,
                        y: 20,
                        width: 14,
                        height: 26
                    })
                    .unwrap()
                    .height,
                    26
                );
            }
        }
    }

    #[test]
    fn clipped_initial_expands_one_baseline_without_absorbing_multiline_art() {
        for extra_line in [false, true] {
            let mut image = RgbImage::from_pixel(120, 90, Rgb([255; 3]));
            for y in 15..45 {
                for x in 18..23 {
                    image.put_pixel(x, y, Rgb([0; 3]));
                }
            }
            for y in (15..20).chain(27..32) {
                for x in 23..33 {
                    image.put_pixel(x, y, Rgb([0; 3]));
                }
            }
            for top in if extra_line { vec![15, 35] } else { vec![35] } {
                for y in top..top + 10 {
                    for x in (38..44).chain(51..57).chain(64..70) {
                        image.put_pixel(x, y, Rgb([0; 3]));
                    }
                }
            }
            if !extra_line {
                for y in 15..19 {
                    for x in 36..120 {
                        image.put_pixel(x, y, Rgb([0; 3]));
                    }
                }
            }
            let mut ink = Ink::new(
                &image,
                Rect {
                    x: 25,
                    y: 10,
                    width: 60,
                    height: 50,
                },
            );
            let lines = ink.lines();
            if extra_line {
                assert_eq!(
                    lines,
                    [
                        Rect {
                            x: 38,
                            y: 15,
                            width: 32,
                            height: 10
                        },
                        Rect {
                            x: 38,
                            y: 35,
                            width: 32,
                            height: 10
                        }
                    ]
                );
            } else {
                assert_eq!(
                    lines,
                    [Rect {
                        x: 18,
                        y: 15,
                        width: 52,
                        height: 30
                    }]
                );
                assert_eq!(
                    ink.tighten(Rect {
                        x: 38,
                        y: 15,
                        width: 32,
                        height: 30
                    }),
                    Some(Rect {
                        x: 38,
                        y: 35,
                        width: 32,
                        height: 10
                    })
                );
            }
        }
    }

    #[test]
    fn disconnected_letter_counters_do_not_form_one_caption_surface() {
        let mut image = RgbImage::from_fn(120, 100, |x, y| {
            let frame = (20..100).contains(&x)
                && (20..78).contains(&y)
                && (!(22..98).contains(&x) || !(22..76).contains(&y) || (60..62).contains(&x));
            let glyphs = ((40..48).contains(&x) || (72..80).contains(&x)) && (42..54).contains(&y);
            Rgb([if frame || glyphs { 0 } else { 255 }; 3])
        });
        let bounds = Rect {
            x: 20,
            y: 20,
            width: 80,
            height: 58,
        };
        for joined in [false, true] {
            if joined {
                for y in 22..76 {
                    for x in 60..62 {
                        image.put_pixel(x, y, Rgb([255; 3]));
                    }
                }
            }
            let ink = Ink::new(&image, bounds);
            let seed = |x, y| ((y - ink.bounds.y) * ink.bounds.width + x - ink.bounds.x) as usize;
            let frame = Component {
                bounds,
                seed: seed(20, 20),
                has_core: true,
            };
            let components = [
                frame,
                Component {
                    bounds: Rect {
                        x: 40,
                        y: 42,
                        width: 8,
                        height: 12,
                    },
                    seed: seed(40, 42),
                    has_core: true,
                },
                Component {
                    bounds: Rect {
                        x: 72,
                        y: 42,
                        width: 8,
                        height: 12,
                    },
                    seed: seed(72, 42),
                    has_core: true,
                },
            ];
            assert_eq!(ink.is_caption_frame(frame, &components, 12), joined);
        }
    }

    #[test]
    fn outlined_oval_is_not_a_rectangular_caption_frame() {
        let image = RgbImage::from_fn(120, 100, |x, y| {
            let dx = (f64::from(x) - 60.) / 40.;
            let dy = (f64::from(y) - 49.) / 29.;
            let r = dx * dx + dy * dy;
            let outline = (0.8..=1.).contains(&r);
            let letters = [45, 58, 71]
                .iter()
                .any(|&left| (left..left + 6).contains(&x))
                && [28, 44, 60].iter().any(|&top| (top..top + 12).contains(&y));
            Rgb([if outline || letters { 0 } else { 255 }; 3])
        });
        let mut ink = Ink::new(
            &image,
            Rect {
                x: 20,
                y: 20,
                width: 80,
                height: 58,
            },
        );
        assert!(ink.lines().iter().any(|r| r.x < 30));
    }

    #[test]
    fn closed_frame_is_separate_from_the_lines_it_encloses() {
        for scale in [1, 2] {
            for inverted in [false, true] {
                let mut image = RgbImage::from_fn(120 * scale, 100 * scale, |x, y| {
                    let (x, y) = (x / scale, y / scale);
                    let frame = (20..100).contains(&x)
                        && (20..78).contains(&y)
                        && (!(22..98).contains(&x) || !(22..76).contains(&y));
                    let letter = [(30, 38), (50, 58), (70, 78)]
                        .iter()
                        .any(|&(a, b)| (a..b).contains(&x))
                        && [28, 45, 62].iter().any(|&top| (top..top + 12).contains(&y));
                    Rgb([if (frame || letter) == inverted {
                        255
                    } else {
                        0
                    }; 3])
                });
                let bounds = Rect {
                    x: 20 * scale,
                    y: 20 * scale,
                    width: 80 * scale,
                    height: 58 * scale,
                };
                let mut ink = Ink::new(&image, bounds);
                assert_eq!(
                    ink.lines(),
                    [28, 45, 62].map(|y| Rect {
                        x: 30 * scale,
                        y: y * scale,
                        width: 48 * scale,
                        height: 12 * scale
                    })
                );
                for y in 40 * scale..45 * scale {
                    for x in 98 * scale..100 * scale {
                        image.put_pixel(x, y, Rgb([if inverted { 0 } else { 255 }; 3]));
                    }
                }
                // The same bounding box with an opening is not an enclosing
                // contour. Keep its tall connected ink for ordinary grouping.
                let mut open = Ink::new(&image, bounds);
                assert!(open.lines().iter().any(|r| r.x == 20 * scale));
            }
        }
    }

    #[test]
    fn caption_frame_does_not_invert_foreground_and_paper() {
        let mut image = RgbImage::from_pixel(80, 60, Rgb([0; 3]));
        for y in 10..45 {
            for x in 10..70 {
                image.put_pixel(x, y, Rgb([250, 240, 180]));
            }
        }
        for y in 20..32 {
            for x in (20..28).chain(35..43) {
                image.put_pixel(x, y, Rgb([0; 3]));
            }
        }
        let mut ink = Ink::new(
            &image,
            Rect {
                x: 12,
                y: 12,
                width: 56,
                height: 31,
            },
        );
        assert_eq!(
            ink.lines(),
            [Rect {
                x: 20,
                y: 20,
                width: 23,
                height: 12
            }]
        );
    }
}
