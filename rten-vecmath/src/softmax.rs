use std::mem::MaybeUninit;

use rten_simd::dispatch::{SimdOp, SimdUnaryOp};
use rten_simd::functional::{simd_fold, simd_map};
use rten_simd::span::SrcDest;
use rten_simd::{SimdFloat, SimdMask};

use crate::Exp;

/// Computes the [softmax][softmax] function over a slice of floats.
///
/// The implementation uses a three-pass approach for numerical stability.
/// See <https://ogunlao.github.io/2020/04/26/you_dont_really_know_softmax.html>.
/// and <https://arxiv.org/abs/2001.04438>.
///
/// [softmax]: <https://en.wikipedia.org/wiki/Softmax_function>
pub struct Softmax<'a> {
    src_dest: SrcDest<'a, f32>,
}

impl<'a> Softmax<'a> {
    /// Construct a softmax operation which reads `input` and writes to to
    /// `output`.
    pub fn new(input: &'a [f32], output: &'a mut [MaybeUninit<f32>]) -> Self {
        Softmax {
            src_dest: (input, output).into(),
        }
    }

    /// Construct a softmax operation which updates `input` in place.
    pub fn new_mut(input: &'a mut [f32]) -> Self {
        Softmax {
            src_dest: input.into(),
        }
    }
}

impl<'a> SimdOp for Softmax<'a> {
    /// The normalized elements.
    type Output = &'a mut [f32];

    #[inline(always)]
    unsafe fn eval<S: SimdFloat>(self) -> Self::Output {
        let max_val = simd_fold(
            self.src_dest.src(),
            S::splat(f32::MIN),
            #[inline(always)]
            |max, x| max.max(x),
        );
        let max_val = max_val.fold_splat(f32::MIN, |max: f32, x: f32| max.max(x));

        // *x = (*x - max_val).exp()
        let mut prev_exp_sum = S::zero();
        let mut exp_sum = S::zero();
        let dest = simd_map(
            self.src_dest,
            #[inline(always)]
            |x: S| {
                let y = Exp::apply(x.sub(max_val));
                prev_exp_sum = exp_sum;
                exp_sum = exp_sum.add(y);
                y
            },
        );

        // Undo the last update to `exp_sum` for unused lanes.
        let remainder = dest.len() % S::LEN;
        if remainder != 0 {
            let remainder_mask = S::Mask::first_n(remainder);
            exp_sum = prev_exp_sum.blend(exp_sum, remainder_mask);
        }

        // *x /= exp_sum
        let exp_sum = exp_sum.fold_splat(0., |sum, x| sum + x);
        let inv_exp_sum = S::one().div(exp_sum);

        simd_map(
            dest.into(),
            #[inline(always)]
            |x: S| x.mul(inv_exp_sum),
        );

        dest
    }
}

#[cfg(test)]
mod tests {
    use rten_simd::dispatch::SimdOp;

    use super::Softmax;
    use crate::testing::{benchmark_op, check_f32s_are_equal_ulps, triples, AsUninit};

    fn reference_softmax(xs: &[f32], ys: &mut [f32]) {
        let max = xs.iter().copied().fold(f32::MIN, |max, x| max.max(x));

        let mut exp_sum = 0.;
        for (x, y) in xs.iter().zip(ys.iter_mut()) {
            *y = (*x - max).exp();
            exp_sum += *y;
        }

        for el in ys.iter_mut() {
            *el /= exp_sum;
        }
    }

    #[test]
    fn test_softmax() {
        // Test against reference values.
        let input = vec![0.1634, 0.8647, 0.6401, 0.8265, 0.0560, 0.2304];
        let expected = &([
            0.11715934, 0.23623686, 0.18871443, 0.2273828, 0.10522857, 0.12527795,
        ]);
        let mut actual = vec![0.; input.len()];

        Softmax::new(&input, actual.as_mut_slice().as_uninit()).dispatch();
        check_f32s_are_equal_ulps(triples(&input, &actual, expected), 1. /* max ULPs */);

        // Test against reference implementation for various lengths.
        for len in 1..20 {
            let input: Vec<f32> = (0..len).map(|x| x as f32 + 0.1).collect();
            let mut expected = vec![0.; input.len()];
            reference_softmax(&input, &mut expected);

            let mut actual = vec![0.; input.len()];
            Softmax::new(&input, actual.as_mut_slice().as_uninit()).dispatch();

            check_f32s_are_equal_ulps(triples(&input, &actual, &expected), 3. /* max ULPs */);
        }
    }

    #[test]
    #[ignore]
    fn bench_softmax() {
        benchmark_op(reference_softmax, |src, dest| {
            Softmax::new(src, dest).dispatch();
        });
    }
}
