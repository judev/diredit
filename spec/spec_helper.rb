source = File.read(File.expand_path("../../diredit", __FILE__))
preamble, source = source.split("#!ruby", 2)
eval(source)
