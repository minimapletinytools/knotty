-- GHC 9.6.2; regenerate from the repository root:
-- runghc compiler/twine/tests/fixtures/string-golden.hs > compiler/twine/tests/fixtures/string-show.tsv
import Data.Char (chr)
import Data.List (intercalate)
main :: IO ()
main = mapM_ emit cases where
  emit ns = putStrLn (intercalate "," (map show ns) ++ "\t" ++ show (map chr ns))
  cases = [] : [[n] | n <- [0..127]] ++ [[233], [233,50], [129426], [14,72], [14,72,50], [133,50], [8203], [8232], [57344], [775], [1114111]]
