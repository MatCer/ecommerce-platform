//! LZMA1 encoding matching the LZMA SDK / lzma1 mode 5 encoder, including
//! its parsing and price-table behavior, for byte-identical PAY by square codes.

// Adapted from lzma1@0.3.0 (LZMA SDK lineage). Upstream license:
// MIT License
//
// Copyright (c) 2023 Filip Seman <filip.seman@pm.me>
//
// Permission is hereby granted, free of charge, to any person obtaining a copy
// of this software and associated documentation files (the "Software"), to deal
// in the Software without restriction, including without limitation the rights
// to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
// copies of the Software, and to permit persons to whom the Software is
// furnished to do so, subject to the following conditions:
//
// The above copyright notice and this permission notice shall be included in all
// copies or substantial portions of the Software.
//
// THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
// IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
// FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
// AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
// LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
// OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
// SOFTWARE.

const FAST_BYTES: usize = 128;
const MAX_MATCH: usize = 273;
// Lookahead is capped at OPTIMUM_SIZE - 1 before creating any successor.
const OPTIMUM_SIZE: usize = 4096;
const INFINITY: u32 = 0x0fff_ffff;
const LITERAL: usize = usize::MAX;

/// Raw LZMA1 stream (no header) exactly as the LZMA SDK encoder in `lzma1` mode 5 writes it.
///
/// Intended for QR payloads of at most 65,535 bytes. The end marker is retained.
pub fn compress_raw(input: &[u8]) -> Vec<u8> {
    Encoder::new(input).encode()
}

const fn probability_prices() -> [u32; 512] {
    let mut prices = [0; 512];
    let mut i = 0;
    while i <= 8 {
        let start = 1 << (8 - i);
        let end = start * 2;
        let mut j = start;
        while j < end {
            prices[j] = (i << 6) + (((end - j) as u32) << 6 >> (8 - i));
            j += 1;
        }
        i += 1;
    }
    prices
}
const PRICES: [u32; 512] = probability_prices();
// Adaptation keeps probabilities in 31..=2017, within the price table.
fn bit_price(prob: u16, bit: usize) -> u32 {
    PRICES[(if bit == 0 { prob } else { 2048 - prob } >> 2) as usize]
}
fn char_state(state: usize) -> usize {
    [0, 0, 0, 0, 1, 2, 3, 4, 5, 6, 4, 5][state]
}
fn match_state(state: usize) -> usize {
    if state < 7 { 7 } else { 10 }
}
fn rep_state(state: usize) -> usize {
    if state < 7 { 8 } else { 11 }
}
fn short_rep_state(state: usize) -> usize {
    if state < 7 { 9 } else { 11 }
}
fn pos_slot(distance: usize) -> usize {
    if distance < 4 {
        return distance;
    }
    let high = usize::BITS as usize - 1 - distance.leading_zeros() as usize;
    (high << 1) + ((distance >> (high - 1)) & 1)
}

struct RangeEncoder {
    low: u64,
    range: u32,
    cache: u8,
    cache_size: usize,
    output: Vec<u8>,
}
impl RangeEncoder {
    fn new() -> Self {
        Self {
            low: 0,
            range: u32::MAX,
            cache: 0,
            cache_size: 1,
            output: Vec::new(),
        }
    }
    fn shift_low(&mut self) {
        let low = self.low as u32;
        let carry = (self.low >> 32) as u8;
        if carry != 0 || low < 0xff00_0000 {
            let mut byte = self.cache;
            for _ in 0..self.cache_size {
                self.output.push(byte.wrapping_add(carry));
                byte = 255;
            }
            self.cache_size = 0;
            self.cache = (low >> 24) as u8;
        }
        self.cache_size += 1;
        self.low = u64::from(low & 0x00ff_ffff) << 8;
    }
    fn bit(&mut self, prob: &mut u16, bit: usize) {
        let bound = (self.range >> 11) * u32::from(*prob);
        if bit == 0 {
            self.range = bound;
            *prob += (2048 - *prob) >> 5;
        } else {
            self.low += u64::from(bound);
            self.range -= bound;
            *prob -= *prob >> 5;
        }
        if self.range < 1 << 24 {
            self.range <<= 8;
            self.shift_low();
        }
    }
    fn tree(&mut self, models: &mut [u16], bits: usize, symbol: usize) {
        let mut node = 1;
        for shift in (0..bits).rev() {
            let bit = (symbol >> shift) & 1;
            self.bit(&mut models[node], bit);
            node = (node << 1) | bit;
        }
    }
    // The SDK uses offset -1 for distance slot 4; node always starts at 1.
    fn reverse_tree(&mut self, models: &mut [u16], offset: isize, bits: usize, mut symbol: usize) {
        let mut node = 1;
        for _ in 0..bits {
            let bit = symbol & 1;
            self.bit(&mut models[(offset + node as isize) as usize], bit);
            node = (node << 1) | bit;
            symbol >>= 1;
        }
    }
    fn direct(&mut self, value: usize, bits: usize) {
        for shift in (0..bits).rev() {
            self.range >>= 1;
            if (value >> shift) & 1 != 0 {
                self.low += u64::from(self.range);
            }
            if self.range < 1 << 24 {
                self.range <<= 8;
                self.shift_low();
            }
        }
    }
}
fn reverse_price(models: &[u16], offset: isize, bits: usize, mut symbol: usize) -> u32 {
    let mut node = 1;
    let mut price = 0;
    for _ in 0..bits {
        let bit = symbol & 1;
        price += bit_price(models[(offset + node as isize) as usize], bit);
        node = (node << 1) | bit;
        symbol >>= 1;
    }
    price
}

struct LengthCoder {
    choice: [u16; 2],
    low: [[u16; 8]; 4],
    mid: [[u16; 8]; 4],
    high: [u16; 256],
}
impl LengthCoder {
    fn new() -> Self {
        Self {
            choice: [1024; 2],
            low: [[1024; 8]; 4],
            mid: [[1024; 8]; 4],
            high: [1024; 256],
        }
    }
    fn encode(&mut self, range: &mut RangeEncoder, symbol: usize, pos: usize) {
        range.bit(&mut self.choice[0], usize::from(symbol >= 8));
        if symbol < 8 {
            range.tree(&mut self.low[pos], 3, symbol);
        } else {
            range.bit(&mut self.choice[1], usize::from(symbol >= 16));
            if symbol < 16 {
                range.tree(&mut self.mid[pos], 3, symbol - 8);
            } else {
                range.tree(&mut self.high, 8, symbol - 16);
            }
        }
    }
    // lzma1 initializes these prices once and never refreshes them while encoding.
    fn price(len: usize) -> u32 {
        let bits = if len < 10 {
            4
        } else if len < 18 {
            5
        } else {
            10
        };
        bits * bit_price(1024, 0)
    }
}

struct LiteralCoder {
    models: [[u16; 768]; 8],
}
impl LiteralCoder {
    fn new() -> Self {
        Self {
            models: [[1024; 768]; 8],
        }
    }
    fn encode(
        &mut self,
        range: &mut RangeEncoder,
        previous: u8,
        matched: bool,
        match_byte: u8,
        symbol: u8,
    ) {
        let models = &mut self.models[(previous >> 5) as usize];
        let mut node = 1;
        let mut matching = matched;
        for shift in (0..8).rev() {
            let bit = usize::from((symbol >> shift) & 1);
            let match_bit = usize::from((match_byte >> shift) & 1);
            let index = node + if matching { (1 + match_bit) << 8 } else { 0 };
            range.bit(&mut models[index], bit);
            node = (node << 1) | bit;
            matching &= bit == match_bit;
        }
    }
    fn price(&self, previous: u8, matched: bool, match_byte: u8, symbol: u8) -> u32 {
        let models = &self.models[(previous >> 5) as usize];
        let mut node = 1;
        let mut matching = matched;
        let mut price = 0;
        for shift in (0..8).rev() {
            let bit = usize::from((symbol >> shift) & 1);
            let match_bit = usize::from((match_byte >> shift) & 1);
            let index = node + if matching { (1 + match_bit) << 8 } else { 0 };
            price += bit_price(models[index], bit);
            node = (node << 1) | bit;
            matching &= bit == match_bit;
        }
        price
    }
}

const fn crc_table() -> [u32; 256] {
    let mut result = [0; 256];
    let mut i = 0;
    while i < 256 {
        let mut value = i as u32;
        let mut bit = 0;
        while bit < 8 {
            value = (value >> 1) ^ if value & 1 != 0 { 0xedb8_8320 } else { 0 };
            bit += 1;
        }
        result[i] = value;
        i += 1;
    }
    result
}
const CRC: [u32; 256] = crc_table();

/// BT4 with the mode-5 hash mask. For QR-sized inputs the 2 MiB dictionary
/// never wraps, so tree nodes can be indexed directly by input position.
struct MatchFinder<'a> {
    input: &'a [u8],
    pos: usize,
    hash: Vec<usize>,
    sons: Vec<[usize; 2]>,
}
impl<'a> MatchFinder<'a> {
    fn new(input: &'a [u8]) -> Self {
        Self {
            input,
            pos: 0,
            hash: vec![0; 66560 + (1 << 20)],
            sons: vec![[0; 2]; input.len() + 1],
        }
    }
    fn byte(&self, relative: isize) -> u8 {
        self.pos
            .checked_add_signed(relative)
            .and_then(|p| self.input.get(p))
            .copied()
            .unwrap_or(0)
    }
    fn match_len(&self, relative: isize, distance: usize, limit: usize) -> usize {
        let Some(start) = self.pos.checked_add_signed(relative) else {
            return 0;
        };
        let Some(back) = start.checked_sub(distance + 1) else {
            return 0;
        };
        let limit = limit.min(self.input.len().saturating_sub(start));
        (0..limit)
            .take_while(|&i| self.input[start + i] == self.input[back + i])
            .count()
    }
    fn available(&self) -> usize {
        self.input.len() - self.pos
    }
    fn advance(&mut self, distances: &mut Vec<usize>) {
        distances.clear();
        let cur = self.pos;
        let limit = FAST_BYTES.min(self.available());
        self.pos += 1;
        if limit < 4 {
            return;
        }
        // Zero is the missing-link sentinel; stored positions are one-based.
        let position = cur + 1;
        let temp = CRC[self.input[cur] as usize] ^ u32::from(self.input[cur + 1]);
        let hash2 = (temp & 1023) as usize;
        let temp = temp ^ (u32::from(self.input[cur + 2]) << 8);
        let hash3 = 1024 + (temp & 65535) as usize;
        let hash4 =
            66560 + ((temp ^ (CRC[self.input[cur + 3] as usize] << 5)) & 0x000f_ffff) as usize;
        let mut candidate = self.hash[hash4];
        let mut match2 = self.hash[hash2];
        let match3 = self.hash[hash3];
        self.hash[hash2] = position;
        self.hash[hash3] = position;
        self.hash[hash4] = position;
        // Hash matches are at most three bytes, strictly below limit >= 4.
        // A full-length tree match exits before either next-byte access.
        let mut max_len = 1;
        if match2 != 0 && self.input[match2 - 1] == self.input[cur] {
            max_len = 2;
            distances.extend([2, position - match2 - 1]);
        }
        if match3 != 0 && self.input[match3 - 1] == self.input[cur] {
            if match3 == match2 {
                distances.truncate(distances.len() - 2);
            }
            max_len = 3;
            distances.extend([3, position - match3 - 1]);
            match2 = match3;
        }
        if !distances.is_empty() && match2 == candidate {
            distances.truncate(distances.len() - 2);
            max_len = 1;
        }
        let mut left = (position, 1);
        let mut right = (position, 0);
        let mut len_left = 0;
        let mut len_right = 0;
        // lzma1 never decrements its search counter on traversal. Preserve that
        // behavior; links still move strictly backwards and terminate.
        loop {
            if candidate == 0 {
                self.sons[left.0][left.1] = 0;
                self.sons[right.0][right.1] = 0;
                break;
            }
            let previous = candidate - 1;
            let mut len = len_left.min(len_right);
            while len < limit && self.input[previous + len] == self.input[cur + len] {
                len += 1;
            }
            if len > max_len {
                max_len = len;
                distances.extend([len, position - candidate - 1]);
                if len == limit {
                    self.sons[right.0][right.1] = self.sons[candidate][0];
                    self.sons[left.0][left.1] = self.sons[candidate][1];
                    break;
                }
            }
            if self.input[previous + len] < self.input[cur + len] {
                self.sons[right.0][right.1] = candidate;
                right = (candidate, 1);
                candidate = self.sons[right.0][right.1];
                len_right = len;
            } else {
                self.sons[left.0][left.1] = candidate;
                left = (candidate, 0);
                candidate = self.sons[left.0][left.1];
                len_left = len;
            }
        }
    }
    fn skip(&mut self, count: usize) {
        let mut discarded = Vec::new();
        for _ in 0..count {
            self.advance(&mut discarded);
        }
    }
}

#[derive(Clone, Copy, Default)]
struct Optimum {
    state: usize,
    price: u32,
    previous: usize,
    back: usize,
    previous_is_char: bool,
    compound: bool,
    previous2: usize,
    back2: usize,
    reps: [usize; 4],
}
impl Optimum {
    fn set(&mut self, price: u32, previous: usize, back: usize) {
        self.price = price;
        self.previous = previous;
        self.back = back;
        self.previous_is_char = false;
    }
}

struct Encoder<'a> {
    finder: MatchFinder<'a>,
    range: RangeEncoder,
    literal: LiteralCoder,
    length: LengthCoder,
    rep_length: LengthCoder,
    is_match: [u16; 192],
    is_rep: [u16; 12],
    rep_g0: [u16; 12],
    rep_g1: [u16; 12],
    rep_g2: [u16; 12],
    rep0_long: [u16; 192],
    slots: [[u16; 64]; 4],
    distances: [u16; 114],
    align: [u16; 16],
    distance_prices: [u32; 128],
    align_prices: [u32; 16],
    match_count: usize,
    align_count: usize,
    state: usize,
    previous_byte: u8,
    reps: [usize; 4],
    matches: Vec<usize>,
    additional: usize,
    longest: Option<usize>,
    optimum: Vec<Optimum>,
    optimum_end: usize,
    optimum_current: usize,
    back: usize,
}
impl<'a> Encoder<'a> {
    fn new(input: &'a [u8]) -> Self {
        let mut result = Self {
            finder: MatchFinder::new(input),
            range: RangeEncoder::new(),
            literal: LiteralCoder::new(),
            length: LengthCoder::new(),
            rep_length: LengthCoder::new(),
            is_match: [1024; 192],
            is_rep: [1024; 12],
            rep_g0: [1024; 12],
            rep_g1: [1024; 12],
            rep_g2: [1024; 12],
            rep0_long: [1024; 192],
            slots: [[1024; 64]; 4],
            distances: [1024; 114],
            align: [1024; 16],
            distance_prices: [0; 128],
            align_prices: [0; 16],
            match_count: 0,
            align_count: 0,
            state: 0,
            previous_byte: 0,
            reps: [0; 4],
            matches: Vec::with_capacity(FAST_BYTES * 2),
            additional: 0,
            longest: None,
            optimum: vec![Optimum::default(); OPTIMUM_SIZE],
            optimum_end: 0,
            optimum_current: 0,
            back: 0,
        };
        result.fill_distance_prices();
        result.fill_align_prices();
        result
    }
    fn fill_distance_prices(&mut self) {
        // The JS cache is keyed only by tree depth and symbol. All six-bit
        // slot prices are cached before any symbol is encoded, for every state.
        for distance in 0..128 {
            let mut price = 6 * bit_price(1024, 0);
            if distance >= 4 {
                let slot = pos_slot(distance);
                let bits = (slot >> 1) - 1;
                let base = (2 | (slot & 1)) << bits;
                price += reverse_price(
                    &self.distances,
                    base as isize - slot as isize - 1,
                    bits,
                    distance - base,
                );
            }
            self.distance_prices[distance] = price;
        }
        self.match_count = 0;
    }
    fn fill_align_prices(&mut self) {
        for symbol in 0..16 {
            self.align_prices[symbol] = reverse_price(&self.align, 0, 4, symbol);
        }
        self.align_count = 0;
    }
    fn position_price(&self, distance: usize, len: usize) -> u32 {
        let price = if distance < 128 {
            self.distance_prices[distance]
        } else {
            let slot = pos_slot(distance);
            (6 + (slot >> 1) - 1 - 4) as u32 * 64 + self.align_prices[distance & 15]
        };
        price + LengthCoder::price(len)
    }
    fn pure_rep_price(&self, index: usize, state: usize, pos: usize) -> u32 {
        if index == 0 {
            bit_price(self.rep_g0[state], 0) + bit_price(self.rep0_long[(state << 4) + pos], 1)
        } else {
            bit_price(self.rep_g0[state], 1)
                + if index == 1 {
                    bit_price(self.rep_g1[state], 0)
                } else {
                    bit_price(self.rep_g1[state], 1) + bit_price(self.rep_g2[state], index - 2)
                }
        }
    }
    fn rep_price(&self, index: usize, len: usize, state: usize, pos: usize) -> u32 {
        LengthCoder::price(len) + self.pure_rep_price(index, state, pos)
    }
    fn read_matches(&mut self) -> usize {
        self.finder.advance(&mut self.matches);
        self.additional += 1;
        if self.matches.is_empty() {
            return 0;
        }
        let count = self.matches.len();
        let len = self.matches[count - 2];
        if len == FAST_BYTES {
            len + self
                .finder
                .match_len(len as isize - 1, self.matches[count - 1], MAX_MATCH - len)
        } else {
            len
        }
    }
    fn skip(&mut self, count: usize) {
        self.finder.skip(count);
        self.additional += count;
    }
    fn extend_optimum(&mut self, end: &mut usize, target: usize) {
        while *end < target {
            *end += 1;
            self.optimum[*end].price = INFINITY;
        }
    }
    fn backward(&mut self, mut cur: usize) -> usize {
        self.optimum_end = cur;
        let mut previous = self.optimum[cur].previous;
        let mut back = self.optimum[cur].back;
        loop {
            if self.optimum[cur].previous_is_char {
                self.optimum[previous].back = LITERAL;
                self.optimum[previous].previous_is_char = false;
                self.optimum[previous].previous = previous - 1;
                if self.optimum[cur].compound {
                    self.optimum[previous - 1].previous_is_char = false;
                    self.optimum[previous - 1].previous = self.optimum[cur].previous2;
                    self.optimum[previous - 1].back = self.optimum[cur].back2;
                }
            }
            let next_previous = self.optimum[previous].previous;
            let next_back = self.optimum[previous].back;
            self.optimum[previous].back = back;
            self.optimum[previous].previous = cur;
            cur = previous;
            previous = next_previous;
            back = next_back;
            if cur == 0 {
                break;
            }
        }
        self.back = self.optimum[0].back;
        self.optimum_current = self.optimum[0].previous;
        self.optimum_current
    }
    fn optimum_length(&mut self, mut position: usize) -> usize {
        if self.optimum_end != self.optimum_current {
            let entry = self.optimum[self.optimum_current];
            let len = entry.previous - self.optimum_current;
            self.back = entry.back;
            self.optimum_current = entry.previous;
            return len;
        }
        self.optimum_current = 0;
        self.optimum_end = 0;
        let main_len = match self.longest.take() {
            Some(len) => len,
            None => self.read_matches(),
        };
        if self.finder.available() + 1 < 2 {
            self.back = LITERAL;
            return 1;
        }
        let mut reps = self.reps;
        let mut rep_lens = [0; 4];
        let mut best_rep = 0;
        for i in 0..4 {
            rep_lens[i] = self.finder.match_len(-1, reps[i], MAX_MATCH);
            if rep_lens[i] > rep_lens[best_rep] {
                best_rep = i;
            }
        }
        if rep_lens[best_rep] >= FAST_BYTES {
            self.back = best_rep;
            let len = rep_lens[best_rep];
            self.skip(len - 1);
            return len;
        }
        if main_len >= FAST_BYTES {
            self.back = self.matches[self.matches.len() - 1] + 4;
            self.skip(main_len - 1);
            return main_len;
        }
        let current_byte = self.finder.byte(-1);
        let match_byte = self.finder.byte(-(reps[0] as isize) - 2);
        if main_len < 2 && current_byte != match_byte && rep_lens[best_rep] < 2 {
            self.back = LITERAL;
            return 1;
        }
        self.optimum[0].state = self.state;
        let pos = position & 3;
        let literal_price = bit_price(self.is_match[(self.state << 4) + pos], 0)
            + self.literal.price(
                self.previous_byte,
                self.state >= 7,
                match_byte,
                current_byte,
            );
        self.optimum[1].set(literal_price, 0, LITERAL);
        let match_price = bit_price(self.is_match[(self.state << 4) + pos], 1);
        let rep_match_price = match_price + bit_price(self.is_rep[self.state], 1);
        if match_byte == current_byte {
            let price = rep_match_price
                + bit_price(self.rep_g0[self.state], 0)
                + bit_price(self.rep0_long[(self.state << 4) + pos], 0);
            if price < self.optimum[1].price {
                self.optimum[1].set(price, 0, 0);
            }
        }
        let mut end = main_len.max(rep_lens[best_rep]);
        if end < 2 {
            self.back = self.optimum[1].back;
            return 1;
        }
        self.optimum[0].reps = reps;
        for len in 2..=end {
            self.optimum[len].price = INFINITY;
        }
        for (index, &len) in rep_lens.iter().enumerate() {
            let price = rep_match_price + self.pure_rep_price(index, self.state, pos);
            for len in (2..=len).rev() {
                let price = price + LengthCoder::price(len);
                if price < self.optimum[len].price {
                    self.optimum[len].set(price, 0, index);
                }
            }
        }
        let normal_price = match_price + bit_price(self.is_rep[self.state], 0);
        let start = if rep_lens[0] >= 2 { rep_lens[0] + 1 } else { 2 };
        let mut pair = 0;
        for len in start..=main_len {
            while len > self.matches[pair] {
                pair += 2;
            }
            let distance = self.matches[pair + 1];
            let price = normal_price + self.position_price(distance, len);
            if price < self.optimum[len].price {
                self.optimum[len].set(price, 0, distance + 4);
            }
        }
        let mut cur = 0;
        loop {
            cur += 1;
            if cur == end {
                return self.backward(cur);
            }
            let mut new_len = self.read_matches();
            let mut pair_count = self.matches.len();
            if new_len >= FAST_BYTES {
                self.longest = Some(new_len);
                return self.backward(cur);
            }
            position += 1;
            let entry = self.optimum[cur];
            let mut previous = entry.previous;
            let mut state;
            if entry.previous_is_char {
                previous -= 1;
                state = if entry.compound {
                    let old = self.optimum[entry.previous2].state;
                    if entry.back2 < 4 {
                        rep_state(old)
                    } else {
                        match_state(old)
                    }
                } else {
                    self.optimum[previous].state
                };
                state = char_state(state);
            } else {
                state = self.optimum[previous].state;
            }
            if previous == cur - 1 {
                state = if entry.back == 0 {
                    short_rep_state(state)
                } else {
                    char_state(state)
                };
            } else {
                let back;
                if entry.previous_is_char && entry.compound {
                    previous = entry.previous2;
                    back = entry.back2;
                    state = rep_state(state);
                } else {
                    back = entry.back;
                    state = if back < 4 {
                        rep_state(state)
                    } else {
                        match_state(state)
                    };
                }
                reps = self.optimum[previous].reps;
                if back < 4 {
                    reps[..=back].rotate_right(1);
                } else {
                    reps = [back - 4, reps[0], reps[1], reps[2]];
                }
            }
            self.optimum[cur].state = state;
            self.optimum[cur].reps = reps;
            let current_byte = self.finder.byte(-1);
            let match_byte = self.finder.byte(-(reps[0] as isize) - 2);
            let pos = position & 3;
            let char_price = entry.price
                + bit_price(self.is_match[(state << 4) + pos], 0)
                + self
                    .literal
                    .price(self.finder.byte(-2), state >= 7, match_byte, current_byte);
            let mut next_is_char = false;
            if char_price < self.optimum[cur + 1].price {
                self.optimum[cur + 1].set(char_price, cur, LITERAL);
                next_is_char = true;
            }
            let match_price = entry.price + bit_price(self.is_match[(state << 4) + pos], 1);
            let rep_match_price = match_price + bit_price(self.is_rep[state], 1);
            let next = self.optimum[cur + 1];
            if match_byte == current_byte && !(next.previous < cur && next.back == 0) {
                let price = rep_match_price
                    + bit_price(self.rep_g0[state], 0)
                    + bit_price(self.rep0_long[(state << 4) + pos], 0);
                if price <= next.price {
                    self.optimum[cur + 1].set(price, cur, 0);
                    next_is_char = true;
                }
            }
            let available = (OPTIMUM_SIZE - 1 - cur).min(self.finder.available() + 1);
            if available < 2 {
                continue;
            }
            let limit = available.min(FAST_BYTES);
            // A literal followed by rep0 can beat either decision in isolation.
            if !next_is_char && match_byte != current_byte {
                let len = self
                    .finder
                    .match_len(0, reps[0], (available - 1).min(FAST_BYTES));
                if len >= 2 {
                    let state2 = char_state(state);
                    let pos2 = (position + 1) & 3;
                    let price = char_price
                        + bit_price(self.is_match[(state2 << 4) + pos2], 1)
                        + bit_price(self.is_rep[state2], 1)
                        + self.rep_price(0, len, state2, pos2);
                    let target = cur + 1 + len;
                    self.extend_optimum(&mut end, target);
                    if price < self.optimum[target].price {
                        let opt = &mut self.optimum[target];
                        opt.set(price, cur + 1, 0);
                        opt.previous_is_char = true;
                        opt.compound = false;
                    }
                }
            }
            let mut start = 2;
            for (index, &distance) in reps.iter().enumerate() {
                let len = self.finder.match_len(-1, distance, limit);
                if len < 2 {
                    continue;
                }
                self.extend_optimum(&mut end, cur + len);
                for test_len in (2..=len).rev() {
                    let price = rep_match_price + self.rep_price(index, test_len, state, pos);
                    if price < self.optimum[cur + test_len].price {
                        self.optimum[cur + test_len].set(price, cur, index);
                    }
                }
                if index == 0 {
                    start = len + 1;
                }
                if len < available {
                    let len2 = self.finder.match_len(
                        len as isize,
                        distance,
                        (available - 1 - len).min(FAST_BYTES),
                    );
                    if len2 >= 2 {
                        let price = rep_match_price + self.rep_price(index, len, state, pos);
                        self.compound_match(
                            CompoundMatch {
                                cur,
                                position,
                                len,
                                len2,
                                distance,
                                back: index,
                                state: rep_state(state),
                                price,
                            },
                            &mut end,
                        );
                    }
                }
            }
            if new_len > limit {
                new_len = limit;
                pair_count = 0;
                while new_len > self.matches[pair_count] {
                    pair_count += 2;
                }
                self.matches[pair_count] = new_len;
                pair_count += 2;
            }
            if new_len >= start {
                let normal_price = match_price + bit_price(self.is_rep[state], 0);
                self.extend_optimum(&mut end, cur + new_len);
                let mut pair = 0;
                while start > self.matches[pair] {
                    pair += 2;
                }
                for len in start..=new_len {
                    let distance = self.matches[pair + 1];
                    let price = normal_price + self.position_price(distance, len);
                    if price < self.optimum[cur + len].price {
                        self.optimum[cur + len].set(price, cur, distance + 4);
                    }
                    if len == self.matches[pair] {
                        if len < available {
                            let len2 = self.finder.match_len(
                                len as isize,
                                distance,
                                (available - 1 - len).min(FAST_BYTES),
                            );
                            if len2 >= 2 {
                                self.compound_match(
                                    CompoundMatch {
                                        cur,
                                        position,
                                        len,
                                        len2,
                                        distance,
                                        back: distance + 4,
                                        state: match_state(state),
                                        price,
                                    },
                                    &mut end,
                                );
                            }
                        }
                        pair += 2;
                        if pair == pair_count {
                            break;
                        }
                    }
                }
            }
        }
    }
    /// Price a match, one literal, then rep0; retain both predecessors so that
    /// backward reconstruction emits all three decisions in their original order.
    fn compound_match(&mut self, path: CompoundMatch, end: &mut usize) {
        let CompoundMatch {
            cur,
            position,
            len,
            len2,
            distance,
            back,
            state,
            price,
        } = path;
        let pos = (position + len) & 3;
        let price = price
            + bit_price(self.is_match[(state << 4) + pos], 0)
            + self.literal.price(
                self.finder.byte(len as isize - 2),
                true,
                self.finder.byte(len as isize - distance as isize - 2),
                self.finder.byte(len as isize - 1),
            );
        let state = char_state(state);
        let pos = (position + len + 1) & 3;
        let price = price
            + bit_price(self.is_match[(state << 4) + pos], 1)
            + bit_price(self.is_rep[state], 1)
            + self.rep_price(0, len2, state, pos);
        let target = cur + len + 1 + len2;
        self.extend_optimum(end, target);
        if price < self.optimum[target].price {
            let opt = &mut self.optimum[target];
            opt.set(price, cur + len + 1, 0);
            opt.previous_is_char = true;
            opt.compound = true;
            opt.previous2 = cur;
            opt.back2 = back;
        }
    }
    fn encode(mut self) -> Vec<u8> {
        let mut position = 0;
        if !self.finder.input.is_empty() {
            self.read_matches();
            self.range.bit(&mut self.is_match[0], 0);
            let byte = self.finder.input[0];
            self.literal.encode(&mut self.range, 0, false, 0, byte);
            self.previous_byte = byte;
            self.additional -= 1;
            position = 1;
        }
        while position < self.finder.input.len() {
            let len = self.optimum_length(position);
            let back = self.back;
            let pos = position & 3;
            let complex = (self.state << 4) + pos;
            if back == LITERAL {
                self.range.bit(&mut self.is_match[complex], 0);
                let byte = self.finder.input[position];
                let match_byte = self
                    .finder
                    .byte(-(self.reps[0] as isize) - 1 - self.additional as isize);
                self.literal.encode(
                    &mut self.range,
                    self.previous_byte,
                    self.state >= 7,
                    match_byte,
                    byte,
                );
                self.state = char_state(self.state);
            } else {
                self.range.bit(&mut self.is_match[complex], 1);
                self.range
                    .bit(&mut self.is_rep[self.state], usize::from(back < 4));
                if back < 4 {
                    self.range
                        .bit(&mut self.rep_g0[self.state], usize::from(back != 0));
                    if back == 0 {
                        self.range
                            .bit(&mut self.rep0_long[complex], usize::from(len != 1));
                    } else {
                        self.range
                            .bit(&mut self.rep_g1[self.state], usize::from(back != 1));
                        if back != 1 {
                            self.range.bit(&mut self.rep_g2[self.state], back - 2);
                        }
                    }
                    if len == 1 {
                        self.state = short_rep_state(self.state);
                    } else {
                        self.rep_length.encode(&mut self.range, len - 2, pos);
                        self.state = rep_state(self.state);
                    }
                    self.reps[..=back].rotate_right(1);
                } else {
                    self.state = match_state(self.state);
                    self.length.encode(&mut self.range, len - 2, pos);
                    let distance = back - 4;
                    let slot = pos_slot(distance);
                    self.range.tree(&mut self.slots[(len - 2).min(3)], 6, slot);
                    if slot >= 4 {
                        let bits = (slot >> 1) - 1;
                        let base = (2 | (slot & 1)) << bits;
                        let reduced = distance - base;
                        if slot < 14 {
                            self.range.reverse_tree(
                                &mut self.distances,
                                base as isize - slot as isize - 1,
                                bits,
                                reduced,
                            );
                        } else {
                            self.range.direct(reduced >> 4, bits - 4);
                            self.range.reverse_tree(&mut self.align, 0, 4, reduced & 15);
                            self.align_count += 1;
                        }
                    }
                    self.reps = [distance, self.reps[0], self.reps[1], self.reps[2]];
                    self.match_count += 1;
                }
            }
            self.previous_byte = self.finder.input[position + len - 1];
            self.additional -= len;
            position += len;
            if self.additional == 0 {
                if self.match_count >= 128 {
                    self.fill_distance_prices();
                }
                if self.align_count >= 16 {
                    self.fill_align_prices();
                }
            }
        }
        // End-of-stream is a length-two match at distance 0xffff_ffff.
        self.range
            .bit(&mut self.is_match[(self.state << 4) + (position & 3)], 1);
        self.range.bit(&mut self.is_rep[self.state], 0);
        self.length.encode(&mut self.range, 0, position & 3);
        self.range.tree(&mut self.slots[0], 6, 63);
        self.range.direct(0x03ff_ffff, 26);
        self.range.reverse_tree(&mut self.align, 0, 4, 15);
        for _ in 0..5 {
            self.range.shift_low();
        }
        self.range.output
    }
}
struct CompoundMatch {
    cur: usize,
    position: usize,
    len: usize,
    len2: usize,
    distance: usize,
    back: usize,
    state: usize,
    price: u32,
}

#[cfg(test)]
mod tests {
    use super::compress_raw;

    fn hex(value: &str) -> Vec<u8> {
        fn digit(byte: u8) -> u8 {
            match byte {
                b'0'..=b'9' => byte - b'0',
                b'a'..=b'f' => byte - b'a' + 10,
                _ => panic!("invalid embedded hex"),
            }
        }
        assert_eq!(value.len() % 2, 0);
        value
            .as_bytes()
            .as_chunks::<2>()
            .0
            .iter()
            .map(|[hi, lo]| digit(*hi) * 16 + digit(*lo))
            .collect()
    }

    // Generated with lzma1@0.3.0 compress(input, 5).subarray(13).
    #[test]
    fn reference_vectors() {
        let vectors = [
            ("", "0083fffbffffc0000000"),
            ("00", "000041fef7ffffe0008000"),
            ("ff", "007fc1fbffffffe0000000"),
            (
                "68656c6c6f20776f726c64",
                "00341949ee8de917893a336005f7cf64fffb782000",
            ),
        ];
        for (input, expected) in vectors {
            assert_eq!(compress_raw(&hex(input)), hex(expected));
        }
    }

    #[test]
    fn repetitive_reference_vector() {
        // 128 copies of the hex-encoded tab-separated pattern: 1,536 bytes.
        let input = hex("616263093132330945555209").repeat(128);
        assert_eq!(input.len(), 1536);
        assert_eq!(
            compress_raw(&input),
            hex("0030988891258e04c758fc94ce6f9fead90354da51c3706cd0a53735ffffeb0a0000")
        );
    }

    // CRC32(payload) in little-endian order + UTF-8 payload. Expected streams
    // are decoded from the five PAY by square QR fixtures, after their 4-byte prefix.
    #[test]
    fn payment_golden_vectors() {
        let vectors = [
            (
                "9a0a90b4093109310931322e39094555520909313030303031090909094f626a65646e61766b6120313030303031093109534b393631313030303030303030323931383539393636390954415452534b4258093009300944656d6f206f6263686f6420732e722e6f2e0909",
                "004d028dac75159d24ce1455bfd976bdbe1bfd42fe355efbce1db583038afbdd383f427a2bf3b0a4b650bcc2d4fd6ce4713abc1abb757223361673bbd05b1585a78323694f220c3024c25e8e131275fc39778a8d7631228e5fe71684261fff988a8000",
            ),
            (
                "9b101ee80931093109313030094555520909313030303032090909094f626a65646e61766b6120313030303032093109534b3331313230303030303031393837343236333735343109093009300944656d6f0909",
                "004d83ffb0183ca487df61f76e0c022ef979fa9a186670434c7f0a1db1272740871665223d360a3c2561d9da2311c386e058863c05eafb78e2ee12e62ba5eb76868ef8dcdd563d5dfffbdc1000",
            ),
            (
                "aa7fce80093109310934322e35094555520909313233090909094c75626f766f6c6e6120706f7a6e616d6b61202a206373747a79616965093109534b393631313030303030303030323931383539393636390954415452534b425858585809300930094c75626f6d69722053746173746e7920e28093205a696c696e610909",
                "00551fd5c8004b0c1b3a5fdfef05172d77a34fb07db58b0d426f3acbab02eee15708af8901896415dfbaede4e55f706631cc34990728866a0b73c626905b2847d84b89d026bccef1a44eed235a599c6e36668d3e5a8704bc955e7d15bb6b8a8b7e13ad7cbe9d07ccbf1f91a245757f5deb1f935f0a75fcfba600",
            ),
            (
                "6bf1d3cf09310931093132333435362e37380945555209093939393939393939393909090909506c61746261207a61206f626a65646e61766b75206369736c6f20393939393939393939392076206f6263686f64652044656d6f206f6263686f64093109534b303830393030303030303030303132333132333132330947494241534b4258093009300956656c6d6920646c6879206e617a6f76206f6263686f646e696b6120732e722e6f2e0909",
                "0035bc566cf048b9f7950d63186d4ab73e1dd033ae3b9c294bcc103ea3d5f80eee9c80dffb41db1ec5b80092096e0acc21f9c712a801396e2c03fcbdcaa34e413a47bcce66716b8c6dd82587de84960931b74b14d3b6b1f23ae3783cfa156d69b08b35cad3ea6f12681529d8dc6fc2747240b4223926d3006e1da6ff512c5647254eadefffe0c16800",
            ),
            (
                "f2364be50931093109302e303509455552090931303030303309090909093109534b393631313030303030303030323931383539393636390954415452534b4258093009300944656d6f0909",
                "00790d856e504d5003d5fa20547a425a919c183b69c5bc59dde672ad45ad5586b4fc4374daaf222a3fea5c2de68dd49b910d1b9ddc056a640b71f9252f1a5ca58bbfbffff8421000",
            ),
        ];
        for (input, expected) in vectors {
            assert_eq!(compress_raw(&hex(input)), hex(expected));
        }
    }
}
