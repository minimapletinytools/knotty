-- Regenerate with GHC 9.6.2 from the repository root:
-- runghc compiler/twine/tests/fixtures/float-golden.hs > compiler/twine/tests/fixtures/float-show.tsv
import Data.List (nub)
import Data.Word (Word64)
import GHC.Float (castDoubleToWord64, castWord64ToDouble)
import Numeric (showHex)

step :: Word64 -> Word64
step x = x * 6364136223846793005 + 1442695040888963407

main :: IO ()
main = mapM_ emit cases where
  emit bits = putStrLn (showHex bits "\t" ++ show (castWord64ToDouble bits))
  cases = nub $ [0, 0x8000000000000000, 1, 2, 0x000fffffffffffff,
                 0x0010000000000000, 0x7fefffffffffffff,
                 0x7ff0000000000000, 0xfff0000000000000, 0x7ff8000000000000]
          ++ [b + offset | v <- [0.01, 0.1, 1, 2, 10, 1e6, 1e7, 1e23, 1e-100, 1e100],
                           let b = castDoubleToWord64 v, offset <- [maxBound, 0, 1]]
          ++ take 256 (iterate step 0x3141592653589793)
