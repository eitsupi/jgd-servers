# Used by real-r-smoke.ts. Only base R and jgd are required.
args <- commandArgs(trailingOnly = TRUE)
stopifnot(length(args) == 2L)
socket <- args[[1L]]
state_dir <- args[[2L]]

checkpoint <- function(name) {
  writeLines(name, file.path(state_dir, paste0(name, ".ready")))
  deadline <- Sys.time() + 45
  while (!file.exists(file.path(state_dir, paste0(name, ".continue")))) {
    if (Sys.time() > deadline) stop("Timed out at checkpoint: ", name)
    Sys.sleep(0.05)
  }
}

cat(R.version.string, "\n")
cat("jgd", as.character(packageVersion("jgd")), "\n")
jgd::jgd(width = 4, height = 3, dpi = 96, socket = socket)
par(mar = c(2, 2, 2, 1))
plot(1:5, c(1, 4, 2, 5, 3), type = "b", main = "Real R baseline")
lines(1:5, c(3, 2, 4, 1, 5), col = "blue")
text(3, 3, "日本語", srt = 30, adj = 0.3)
checkpoint("first")

plot.new()
plot.window(xlim = c(0, 1), ylim = c(0, 1))
rect(0.1, 0.1, 0.9, 0.9, col = "#33669980", border = "red")
text(0.5, 0.5, "Second page")
checkpoint("second")
dev.off()
