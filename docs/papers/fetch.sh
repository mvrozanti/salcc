#!/bin/sh
# Downloads the open-access PDFs cited in docs/research.md and README.md into
# docs/papers/. Idempotent: existing files are skipped. Only license-safe
# copies (arXiv, author-hosted, ERIC, government) are fetched; paywalled works
# are listed at the bottom with citation + DOI and are NOT downloaded.
# Last verified: 2026-07-28.

set -u

cd "$(dirname "$0")" || exit 1

fetch() {
    url=$1
    out=$2
    if [ -f "$out" ]; then
        printf 'skip %s (already present)\n' "$out"
        return 0
    fi
    if curl -fsSL --retry 2 --max-time 180 \
        -A "Mozilla/5.0 (X11; Linux x86_64)" -o "$out.part" "$url"; then
        mv "$out.part" "$out"
        printf 'ok   %s\n' "$out"
    else
        rm -f "$out.part"
        printf 'FAIL %s <- %s\n' "$out" "$url" >&2
        failed=1
    fi
}

failed=0

# Vasconcelos, H., Bansal, G., Fourney, A., Liao, Q. V., & Wortman Vaughan, J. (2025). Generation Probabilities Are Not Enough: Uncertainty Highlighting in AI Code Completions. ACM Transactions on Computer-Human Interaction (presented at CHI'25). arXiv:2302.07248.
fetch "https://arxiv.org/pdf/2302.07248" "vasconcelos-2025-generation-probabilities.pdf"

# Gehrmann, S., Strobelt, H., & Rush, A. M. (2019). GLTR: Statistical Detection and Visualization of Generated Text. ACL 2019 System Demonstrations. arXiv:1906.04043.
fetch "https://arxiv.org/pdf/1906.04043" "gehrmann-2019-gltr.pdf"

# Ahmed, S. S., Wang, S., Tian, Y., Chen, T.-H., & Zhang, H. (2024). Studying and Recommending Information Highlighting in Stack Overflow Answers. arXiv:2401.01472.
fetch "https://arxiv.org/pdf/2401.01472" "ahmed-2024-so-highlighting.pdf"

# Warden, A. C., Wickens, C. D., Rehberg, D., Ortega, F. R., & Clegg, B. A. (2023). Fast, Accurate, but Sometimes Too-Compelling Support: The Impact of Imperfectly Automated Cues in an Augmented Reality Head-Mounted Display on Visual Search Performance. arXiv:2303.14300.
fetch "https://arxiv.org/pdf/2303.14300" "warden-2023-imperfect-ar-cueing.pdf"

# Busjahn, T., Bednarik, R., Begel, A., Crosby, M., Paterson, J. H., Schulte, C., Sharif, B., & Tamm, S. (2015). Eye Movements in Code Reading: Relaxing the Linear Order. ICPC 2015 (author's version, Glasgow Caledonian University).
fetch "https://researchonline.gcu.ac.uk/ws/files/24953094/ICPC2015_authors_version.pdf" "busjahn-2015-eye-movements-code-reading.pdf"

# Sarkar, A. (2015). The impact of syntax colouring on program comprehension. PPIG 2015 (26th Annual Workshop of the Psychology of Programming Interest Group).
fetch "https://ppig.org/files/2015-PPIG-26th-Sarkar1.pdf" "sarkar-2015-syntax-colouring.pdf"

# Gier, V. S., Kreiner, D. S., & Natz-Gonzalez, A. (2009). Harmful Effects of Preexisting Inappropriate Highlighting on Reading Comprehension and Metacognitive Accuracy. The Journal of General Psychology, 136(3), 287-302. ERIC EJ926361.
fetch "https://files.eric.ed.gov/fulltext/EJ926361.pdf" "gier-2009-inappropriate-highlighting.pdf"

# Centre for Education Statistics and Evaluation (2017). Cognitive load theory: Research that teachers really need to understand. NSW Department of Education.
fetch "https://education.nsw.gov.au/content/dam/main-education/about-us/educational-data/cese/2017-cognitive-load-theory.pdf" "cese-2017-cognitive-load-theory.pdf"

# --- Open-access in principle, but no fetchable copy found (2026-07-28) ---
#
# Healey, C. G. (1996). Choosing Effective Colours for Data Visualization.
# Proceedings of IEEE Visualization '96, 263-270.
#   https://vis.cs.brown.edu/docs/pdf/Healey-1996-CEC.pdf  (host unreachable)
#   https://www.csc2.ncsu.edu/faculty/healey/download/cec.96.pdf  (403 bot-blocked)
# fetch "https://vis.cs.brown.edu/docs/pdf/Healey-1996-CEC.pdf" "healey-1996-choosing-effective-colours.pdf"
#
# Cowan, N. (2001). The magical number 4 in short-term memory: A
# reconsideration of mental storage capacity. Behavioral and Brain Sciences,
# 24(1), 87-114. doi:10.1017/S0140525X01003922
#   https://philpapers.org/archive/COWTMN-2.pdf  (403 bot-blocked)
# fetch "https://philpapers.org/archive/COWTMN-2.pdf" "cowan-2001-magical-number-4.pdf"

# --- Paywalled: cite only, cannot be redistributed in a public repo ---
#
# Fok, R., Kambhamettu, H., Soldaini, L., Bragg, J., Lo, K., Hearst, M.,
#   Head, A., & Weld, D. S. (2023). Scim: Intelligent Skimming Support for
#   Scientific Papers. IUI 2023. doi:10.1145/3581641.3584034
# Lo, K., et al. (2024). The Semantic Reader Project. Communications of the
#   ACM. doi:10.1145/3665648
# Treisman, A. M., & Gelade, G. (1980). A feature-integration theory of
#   attention. Cognitive Psychology, 12(1), 97-136.
#   doi:10.1016/0010-0285(80)90005-5
# Duncan, J., & Humphreys, G. W. (1989). Visual search and stimulus
#   similarity. Psychological Review, 96(3), 433-458.
#   doi:10.1037/0033-295X.96.3.433
# Dunlosky, J., Rawson, K. A., Marsh, E. J., Nathan, M. J., & Willingham,
#   D. T. (2013). Improving Students' Learning With Effective Learning
#   Techniques. Psychological Science in the Public Interest, 14(1), 4-58.
#   doi:10.1177/1529100612453266
# Ponce, H. R., Mayer, R. E., & Mendez, E. E. (2022). Effects of
#   Learner-Generated Highlighting and Instructor-Provided Highlighting on
#   Learning from Text: A Meta-Analysis. Educational Psychology Review, 34,
#   989-1024. doi:10.1007/s10648-021-09654-1
# Alpizar, D., Adesope, O. O., & Wong, R. M. (2020). A meta-analysis of
#   signaling principle in multimedia learning environments. Educational
#   Technology Research and Development, 68, 2095-2119.
#   doi:10.1007/s11423-020-09748-7
# Schneider, S., Beege, M., Nebel, S., & Rey, G. D. (2018). A meta-analysis
#   of how signaling affects learning with media. Educational Research
#   Review, 23, 1-24. doi:10.1016/j.edurev.2017.11.001
# Wickens, C. D., & Dixon, S. R. (2007). The benefits of imperfect diagnostic
#   automation: a synthesis of the literature. Theoretical Issues in
#   Ergonomics Science, 8(3), 201-212. doi:10.1080/14639220500370105

exit "$failed"
