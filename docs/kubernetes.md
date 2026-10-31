# Kubernetes in a Puddle workspace

kind, k3d and minikube start inside a workspace, but Kubernetes Services do not route.

## What works and what does not

A cluster comes up and its pods run. A Service does not reach its pods: its virtual address
answers nothing. Every workspace uses the same guest kernel, and that kernel lacks the netfilter
pieces `kube-proxy` uses to build Services (`xt_statistic`, `nft numgen`) and the IPVS modules.
An image cannot bring its own kernel.

## What to do

- Reach a pod by its own address, or forward a port to it (`kubectl port-forward`), instead of
  going through a Service.
- Run the cluster outside the workspace when the work needs Services.

## The warning

Puddle looks for `kind`, `k3d` and `minikube` on the workspace's `PATH` each time the workspace
starts. When it finds one, the workspace gets a notice that names the tools and links here. The
notice goes away when the workspace stops, and the next start checks again, so a tool installed
later shows from the next start.

Running Services in the workspace's kernel is planned.
